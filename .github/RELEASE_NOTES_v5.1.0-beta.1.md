Batly 5.1.0-beta.1 : l'éclairage RGB de vos périphériques Logitech, jusque dans leur mémoire.

> **Préversion expérimentale.** Elle n'est pas proposée par la mise à jour automatique : installez-la à la main. La fonction d'enregistrement dans l'appareil n'a été essayée qu'avec un périphérique simulé.

## Nouveau : Éclairage
Menu → *Récepteurs et appareils…* → sélectionnez l'appareil → section **Éclairage**.
- **Zones** (principale, logo, côtés…) et **effets** : couleur fixe, respiration, cycle de couleurs, éteint ; **couleur** au choix et **vitesse** de l'effet.
- **Aperçu** : l'effet s'affiche tout de suite. Batly pilote les LEDs jusqu'à **Rendre la main** ou la fermeture de la fenêtre.
- **Enregistrer dans l'appareil** : l'éclairage est écrit dans le **profil actif de la mémoire embarquée**, comme avec le gestionnaire de mémoire de G HUB. L'appareil le garde ensuite sans aucun logiciel, sur n'importe quel PC.
- **Diagnostic** : un rapport en lecture seule (zones, effets, mémoire) à joindre à une issue si votre appareil n'est pas encore pris en charge.

## Sécurité de l'enregistrement
- Uniquement les **souris** dont la mémoire correspond au format connu : taille, **CRC** et contenu sont vérifiés avant toute écriture ; sinon Batly refuse et dit pourquoi.
- Les **profils d'usine** (lecture seule) ne sont jamais modifiés.
- La mémoire est **sauvegardée** avant chaque écriture dans `%APPDATA%\Batly\backups\`, puis **relue** après : en cas d'écart, le contenu précédent est réécrit.
- Claviers : aperçu seulement pour l'instant (leur format de mémoire est différent). Lancez *Diagnostic* et envoyez le rapport pour les faire prendre en charge.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.1.0-beta.1.exe` | Installateur |
| `Batly-5.1.0-beta.1-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |

Windows 10 (1607) ou plus récent, x64. La version finale 5.1.0 sera proposée automatiquement aux installations de cette bêta.
