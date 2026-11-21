Première version de la réécriture en Rust de LGSTrayBattery : niveau de batterie des périphériques Logitech (souris, clavier, casque) dans la zone de notification de Windows.

## Points forts
- Un seul exécutable natif (~1 Mo) : pas de .NET, pas de `hidapi.dll`, pas de processus auxiliaire
- Entièrement événementiel : changement de charge visible immédiatement, aucun réveil périodique
- Bluetooth LE et G HUB gérés
- Jauge continue colorée ou affichage en %, thème clair/sombre, DPI
- Notifications de batterie faible (seuil réglable)
- API HTTP compatible avec l'original (XML) + JSON
- Installateur par utilisateur, sans droits administrateur (`/S` pour silencieux)
- Interface en français et en anglais

## Téléchargements
| Fichier | Usage |
|---|---|
| `LGSTray-Setup-4.0.0.exe` | Installateur (recommandé) |
| `LGSTray-4.0.0-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |

Windows 10 (1607) ou plus récent, x64.

> ⚠️ La couche Win32 n'a pas encore été testée sur du vrai matériel. En cas de problème, lancez `lgstray.exe --debug` et joignez `lgstray.log` à une issue.
