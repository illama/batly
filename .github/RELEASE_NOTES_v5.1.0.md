Batly 5.1.0 : l'éclairage RGB de vos périphériques Logitech, jusque dans leur mémoire.

## Nouveau : Éclairage
Menu → *Récepteurs et appareils…* → sélectionnez l'appareil → section **Éclairage**.
- **Zones, effets, couleur et vitesse** : couleur fixe, respiration, cycle de couleurs ou éteint, pour les périphériques Logitech LIGHTSYNC.
- **Aperçu** : l'effet s'affiche tout de suite ; **Rendre la main** (ou la fermeture de la fenêtre) le rend à l'appareil.
- **Enregistrer dans l'appareil…**
  - **Souris avec mémoire embarquée** (vérifié sur G502 X PLUS) : l'éclairage est écrit dans le **profil actif**, comme avec le gestionnaire de mémoire de G HUB. La souris le garde ensuite sans aucun logiciel, sur n'importe quel PC, veille comprise.
  - **Claviers et appareils sans mémoire** (vérifié sur PRO X 60) : **Batly garde l'éclairage** et le réapplique à chaque connexion ou réveil, tant que Batly tourne. Les LEDs s'éteignent toujours en fondu à l'inactivité, et sont rendues à l'appareil à la fermeture de Batly.
- **Restaurer…** : remet l'éclairage d'origine (souris) ou arrête de le garder (claviers).
- **Diagnostic** : rapport en lecture seule pour aider à prendre en charge un nouvel appareil.

## Sécurité de l'écriture en mémoire
- Taille, CRC et format sont vérifiés avant toute écriture ; un profil d'usine ou un format inconnu n'est jamais modifié.
- La mémoire est **sauvegardée** avant chaque écriture dans `%APPDATA%\Batly\backups\`, puis **relue** : en cas d'écart, le contenu précédent est réécrit.
- Seule la partie éclairage du profil est touchée : DPI, boutons et autres réglages restent tels quels.

## Correctifs
- **Réponses mélangées** : une réponse tardive d'un périphérique ne peut plus être prise pour la réponse à la question suivante.
- **Identifiant à zéro** : les appareils qui annoncent un numéro de série vide (G502 X PLUS) utilisent leur identifiant matériel. Pour eux, l'historique et les réglages propres à l'appareil repartent de zéro une fois.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.1.0.exe` | Installateur (recommandé) |
| `Batly-5.1.0-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |

Windows 10 (1607) ou plus récent, x64. Les installations 5.0.x et les préversions 5.1 proposeront la mise à jour automatiquement.

Merci aux projets [Solaar](https://github.com/pwr-Solaar/Solaar) et [libratbag](https://github.com/libratbag/libratbag), dont la documentation du protocole HID++ a rendu cette version possible.
