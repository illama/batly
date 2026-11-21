# Intégrations Batly

Toutes s'appuient sur l'API HTTP locale de Batly (`http://localhost:12321` par défaut).

| Dossier / fonction | Usage |
|---|---|
| [`rainmeter/`](rainmeter/README.md) | skin Rainmeter (bureau Windows) |
| [`streamdeck/`](streamdeck/README.md) | plugin Elgato Stream Deck |
| `/overlay` | source navigateur pour OBS : `http://localhost:12321/overlay?device=G502&size=32&theme=dark&layout=row` |
| `/metrics` | Prometheus / Grafana |
| `[MQTT]` | Home Assistant (découverte automatique) et tout broker MQTT |
| `[Webhooks]` | Discord, Slack ou toute URL recevant du JSON |

## Overlay OBS

Ajouter une *Source navigateur* avec l'URL `http://localhost:12321/overlay` (fond transparent,
mise à jour en direct). Paramètres :

| Paramètre | Valeurs | Défaut |
|---|---|---|
| `device` | identifiant, nom ou alias d'un périphérique | tous les périphériques à batterie |
| `size` | taille du texte en pixels (8–400) | `32` |
| `theme` | `dark` (texte blanc, pour scène sombre) ou `light` | `dark` |
| `layout` | `row` (en ligne) ou `column` | `row` |
| `names` | `0` pour masquer les noms | affichés |
| `token` | jeton, si la page est ouverte depuis un autre PC | — |

## API

| Route | Réponse |
|---|---|
| `GET /api/devices`, `/api/device/{id ou nom}` | JSON (+ `alias`, `time_left_hours`, `time_to_full_hours`, `health_percent`, `cycles`, `host`) |
| `GET /api/events` | Server-Sent Events : `hello`, puis un `device` par périphérique et à chaque changement (`{"id":…,"removed":true}` s'il disparaît), `synced` après l'état initial, `: ping` toutes les 25 s |
| `GET /api/history/{id}?days=7` | relevés et statistiques |
| `GET /metrics` | format Prometheus |
| `?local=1` | exclut les périphériques venant d'autres instances (utilisé entre pairs) |

Depuis un autre ordinateur, si `[HTTPServer] token` est défini : `?token=…` ou en-tête
`Authorization: Bearer …`. Les requêtes de la machine locale n'en ont jamais besoin.

Pages web : seules les pages servies depuis cet ordinateur (`http://localhost:…`,
`http://127.0.0.1:…`) reçoivent l'en-tête CORS et peuvent lire l'API depuis le navigateur ; un
site quelconque ne le peut pas. Sans jeton, l'en-tête `Host` doit désigner cet ordinateur
(`localhost`, une adresse IP, ou son nom, éventuellement suivi d'un domaine local comme
`pc-salon.local`) : cela bloque le *DNS rebinding*. Pour joindre l'API par un autre nom
(alias DNS), définissez un jeton.
