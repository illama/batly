Batly 5.5.2 : Batly s'efface complètement devant G HUB.

## Correctif
- **Bandeau G HUB « Les paramètres ne s'appliqueront pas… » et touches inactives** : même sans toucher à l'éclairage, Batly interrogeait les périphériques Logitech en même temps que G HUB, et leurs réponses pouvaient se croiser. Désormais, **tant que G HUB est ouvert, Batly ne communique plus du tout avec les périphériques Logitech** :
  - les batteries sont lues auprès de G HUB (source « G HUB », activée par défaut) ;
  - chaque appareil garde son nom, son icône, sa place et son historique ;
  - Batly reprend la lecture directe dès que G HUB est fermé ;
  - « Récepteurs et appareils » l'explique ; les réglages (DPI, éclairage, Home Assistant) de ces appareils reviennent quand G HUB est fermé.
- La source G HUB relit la liste des appareils toutes les minutes.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.5.2.exe` | Installateur (recommandé) |
| `Batly-5.5.2-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
| `SHA256SUMS.txt.minisig` | Signature des sommes (clé : `packaging/batly-minisign.pub`) |

Windows 10 (1607) ou plus récent, x64. Les installations 5.x proposeront la mise à jour automatiquement.
