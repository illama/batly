Batly 5.1.0-beta.3 : l'éclairage enregistré s'affiche enfin sur la G502 X PLUS, et un bouton pour revenir à l'origine.

> **Préversion expérimentale**, non proposée par la mise à jour automatique.

## Éclairage
- **G502 X PLUS (et profils au format « G502 X »)** : ces souris jouent une *animation embarquée* (l'arc-en-ciel d'usine) à la place des réglages d'éclairage du profil tant qu'un index d'animation est actif. Batly le désactive désormais en enregistrant, comme libratbag : la couleur ou l'effet choisi s'affiche réellement. Avant, on obtenait une vague ultra rapide ou des LEDs éteintes.
- **Restaurer…** : remet l'éclairage qu'avait l'appareil avant la première modification par Batly (depuis la plus ancienne sauvegarde de `backups\`). Seul l'éclairage est restauré : DPI, boutons et autres réglages restent tels quels.
- Les sauvegardes ne s'écrasent plus jamais, même pour deux écritures dans la même seconde.
- **Diagnostic** : lit aussi, en lecture seule, la configuration persistante (« NvConfig ») des claviers RGB, pour étudier l'enregistrement sur clavier.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.1.0-beta.3.exe` | Installateur |
| `Batly-5.1.0-beta.3-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
