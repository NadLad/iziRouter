use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response, sse::{Event, Sse}},
    routing::{get, post},
    Json, Router,
};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use tokio_stream::StreamExt;
use tracing::{debug, info};

// ── Config ──────────────────────────────────────────────────────────────

struct AppConfig {
    flash_model: String,
    pro_model: String,
    api_key: String,
    api_base: String,
    port: u16,
}

impl AppConfig {
    fn from_env() -> Self {
        Self {
            flash_model: std::env::var("FLASH_MODEL")
                .unwrap_or_else(|_| "deepseek-v4-flash".into()),
            pro_model: std::env::var("PRO_MODEL")
                .unwrap_or_else(|_| "deepseek-v4-pro".into()),
            api_key: std::env::var("DEEPSEEK_API_KEY")
                .expect("DEEPSEEK_API_KEY doit être défini"),
            api_base: std::env::var("DEEPSEEK_API_BASE")
                .unwrap_or_else(|_| "https://api.deepseek.com".into()),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8001".into())
                .parse()
                .expect("PORT invalide"),
        }
    }
}

// ── OpenAI-compatible types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChatRequest {
    messages: Vec<Message>,
    stream: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
struct Message {
    role: String,
    content: MessageContent,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    MultiPart(Vec<ContentPart>),
}

impl MessageContent {
    fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::MultiPart(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: serde_json::Value },
}

// ── Classifieur de complexité ──────────────────────────────────────────

const COMPLEX_KEYWORDS: &[&str] = &[
    "explique", "explain", "pourquoi", "why",
    "compare", "compare", "analyse", "analyze",
    "architecture", "design pattern", "refactor",
    "optimise", "optimize", "debug", "débug",
    "implémente", "implement", "conçois", "concept",
    "sécurité", "security", "performance",
    "algorithme", "algorithm", "complexité", "complexity",
    "migration", "base de données", "database",
    "distribué", "distributed", "concurrent",
    "async", "asynchrone", "multi-thread",
    "memory leak", "race condition", "deadlock",
    "ci/cd", "pipeline", "docker", "kubernetes",
];

fn classify_complexity(messages: &[Message]) -> (bool, String) {
    let full_text: String = messages
        .iter()
        .map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join(" ");
    let lower = full_text.to_lowercase();
    let len = full_text.len();

    let mut score: u32 = 0;
    let mut reasons: Vec<&str> = Vec::new();

    // Règle 1: longueur
    if len > 2000 {
        score += 3;
        reasons.push("très long");
    } else if len > 800 {
        score += 2;
        reasons.push("long");
    } else if len > 300 {
        score += 1;
        reasons.push("moyen");
    }

    // Règle 2: présence de code
    let code_markers = [
        "```", "fn ", "def ", "class ", "import ", "pub fn", "impl ",
        "struct ", "trait ", "use ", "mod ", "package ", "require(",
        "from ", "<?php", "#!/", "SELECT ", "INSERT ",
    ];
    let code_count = code_markers.iter().filter(|m| full_text.contains(*m)).count();
    if code_count >= 3 {
        score += 3;
        reasons.push("code dense");
    } else if code_count >= 1 {
        score += 2;
        reasons.push("contient du code");
    }

    // Règle 3: mots-clés de complexité
    let kw_count = COMPLEX_KEYWORDS.iter().filter(|kw| lower.contains(*kw)).count();
    if kw_count >= 4 {
        score += 3;
        reasons.push("très technique");
    } else if kw_count >= 2 {
        score += 2;
        reasons.push("technique");
    } else if kw_count >= 1 {
        score += 1;
    }

    // Règle 4: image → toujours Pro (VL)
    let has_image = messages.iter().any(|m| {
        matches!(&m.content, MessageContent::MultiPart(parts) if parts.iter().any(|p| matches!(p, ContentPart::ImageUrl { .. })))
    });
    if has_image {
        score += 5;
        reasons.push("contient une image");
    }

    // Règle 5: question ouverte en fin de prompt ?
    if let Some(last) = messages.last() {
        let txt = last.content.as_text();
        if txt.contains('?')
            && (txt.contains("quoi")
                || txt.contains("comment")
                || txt.contains("pourquoi")
                || txt.contains("how")
                || txt.contains("why"))
        {
            score += 1;
        }
    }

    let is_complex = score >= 3;
    let reason = if reasons.is_empty() {
        "simple".into()
    } else {
        reasons.join(", ")
    };

    debug!(score, reason, is_complex, "Classification");
    (is_complex, reason)
}

// ── Proxy vers DeepSeek ────────────────────────────────────────────────

async fn forward_request(
    client: &Client,
    config: &AppConfig,
    body: serde_json::Value,
    model: &str,
) -> anyhow::Result<Response> {
    let mut body = body;
    body["model"] = serde_json::Value::String(model.to_string());

    let url = format!("{}/v1/chat/completions", config.api_base);
    let stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("DeepSeek error {}: {}", status.as_u16(), text);
    }

    if stream {
        let byte_stream = resp.bytes_stream();
        let sse_stream = byte_stream.map(|chunk| {
            let bytes = chunk.unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes).to_string();
            Ok::<_, std::convert::Infallible>(Event::default().data(text))
        });
        Ok(Sse::new(sse_stream).into_response())
    } else {
        let text = resp.text().await?;
        Ok(Json(serde_json::from_str::<serde_json::Value>(&text)?).into_response())
    }
}

// ── Handlers ───────────────────────────────────────────────────────────

async fn health() -> &'static str {
    "OK — iziRouter"
}

async fn chat_completions(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let req: ChatRequest = match serde_json::from_value(body.clone()) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Requête invalide: {}", e)})),
            )
                .into_response();
        }
    };

    let (is_complex, reason) = classify_complexity(&req.messages);
    let chosen_model = if is_complex {
        &state.config.pro_model
    } else {
        &state.config.flash_model
    };

    info!(
        model = chosen_model,
        reason,
        stream = req.stream.unwrap_or(false),
        "→ Routage"
    );

    match forward_request(&state.client, &state.config, body, chosen_model).await {
        Ok(mut response) => {
            response.headers_mut().insert(
                "X-iziRouter-Model",
                chosen_model.parse().unwrap(),
            );
            response
                .headers_mut()
                .insert("X-iziRouter-Reason", reason.parse().unwrap());
            response
        }
        Err(e) => {
            tracing::error!("Erreur proxy: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("Erreur proxy: {}", e)})),
            )
                .into_response()
        }
    }
}

struct AppState {
    client: Client,
    config: AppConfig,
}

// ── Main ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,izi_router=debug".into()),
        )
        .init();

    let config = AppConfig::from_env();

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let port = config.port;
    let state = Arc::new(AppState { client, config });

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    info!("🚀 iziRouter démarré sur http://{}", addr);
    info!(
        "   FLASH_MODEL = {}",
        std::env::var("FLASH_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into())
    );
    info!(
        "   PRO_MODEL   = {}",
        std::env::var("PRO_MODEL").unwrap_or_else(|_| "deepseek-v4-pro".into())
    );

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
