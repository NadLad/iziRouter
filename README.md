# iziRouter 🦀

**Mini-proxy OpenAI-compatible en Rust** qui route automatiquement tes requêtes vers
le modèle le plus adapté : DeepSeek Flash pour les questions simples, DeepSeek Pro
pour les questions complexes.

Pas de GPU, pas de base de données, pas d'enfer YAML. Un binaire, une clé API, c'est tout.

## Pourquoi ?

DeepSeek V4-Flash coûte **$0.14/M tokens** en input, V4-Pro coûte **$0.43/M**.
Mais 80% de tes prompts ne nécessitent pas Pro. iziRouter analyse chaque requête
et choisit le bon modèle automatiquement — tu économises sans y penser.

## Installation

```bash
git clone https://github.com/nader/iziRouter.git
cd iziRouter
cp .env.example .env
# Édite .env et mets ta clé DEEPSEEK_API_KEY

cargo build --release
```

## Utilisation

```bash
# Lance le proxy
DEEPSEEK_API_KEY=sk-ta-cle ./target/release/izi-router

# Dans ton client (OpenCrabs, Continue, etc.), pointe l'API sur :
#   base_url = "http://localhost:8001/v1"
#   api_key  = n'importe quoi (iziRouter ne vérifie pas)
```

## Variables d'environnement

| Variable | Défaut | Description |
|---|---|---|
| `DEEPSEEK_API_KEY` | *(obligatoire)* | Ta clé API DeepSeek |
| `FLASH_MODEL` | `deepseek-v4-flash` | Modèle pour les requêtes simples |
| `PRO_MODEL` | `deepseek-v4-pro` | Modèle pour les requêtes complexes |
| `DEEPSEEK_API_BASE` | `https://api.deepseek.com` | URL de base de l'API DeepSeek |
| `PORT` | `8001` | Port d'écoute |
| `RUST_LOG` | `info,izi_router=debug` | Niveau de log |

## Comment ça route ?

Le classifieur attribue un score selon 5 règles :

| Règle | Score max |
|---|---|
| Longueur du prompt (>300, >800, >2000 caractères) | +3 |
| Présence de code (```, fn, class, import, SELECT…) | +3 |
| Mots-clés techniques (explique, debug, architecture, async, docker…) | +3 |
| Contient une image | +5 |
| Question ouverte (pourquoi/comment en fin de prompt) | +1 |

Score ≥ 3 → **Pro** / Score < 3 → **Flash**

Les headers `X-iziRouter-Model` et `X-iziRouter-Reason` sont ajoutés à la réponse
pour que tu saches quel modèle a été choisi et pourquoi.

## API

Compatible OpenAI — les clients existants fonctionnent sans modification.

| Endpoint | Méthode | Description |
|---|---|---|
| `/health` | GET | Health check |
| `/v1/chat/completions` | POST | Chat completions (streaming supporté) |

## Licence

MIT
