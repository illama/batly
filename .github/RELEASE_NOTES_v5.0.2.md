Batly 5.0.2 : plus de périphérique « hors ligne » à tort.

## Correctifs
- **Coupure radio brève** : un clavier ou une souris sans fil pouvait rester affiché « hors ligne » alors qu'il fonctionnait, jusqu'à *Actualiser maintenant*. Après une micro-coupure, le récepteur ne prévient pas toujours que la liaison est revenue ; Batly vérifie désormais de lui-même (au bout de 10 s, puis de plus en plus espacé, jusqu'à toutes les 2 min) et le remet en ligne dès qu'il répond.
- **Récepteur interrompu** : si la connexion à un récepteur s'interrompt (erreur de lecture, sortie de veille…), Batly la rouvre en moins de 30 s, sans attendre qu'un périphérique soit branché.
- **Un appareil relié de deux façons** (récepteur + câble, ou récepteur + Bluetooth) ne passe plus « hors ligne » quand l'un des deux chemins le perd ; les réglages (DPI, rétroéclairage…) passent par le chemin actif.

Un souci de ce genre ? Lancez `batly.exe --debug` : le journal `%APPDATA%\Batly\batly.log` enregistre la prochaine occurrence (taille limitée), à joindre via *Signaler un problème*.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.0.2.exe` | Installateur (recommandé) |
| `Batly-5.0.2-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |

Windows 10 (1607) ou plus récent, x64. Batly 5.0.x proposera la mise à jour automatiquement.
