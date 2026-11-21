Batly 5.1.0-beta.2 : l'éclairage enregistré dans l'appareil, ajusté sur une vraie G502 X PLUS.

> **Préversion expérimentale**, non proposée par la mise à jour automatique.

## Éclairage
- **Souris à une seule zone LED** (G502 X PLUS…) : *Enregistrer dans l'appareil* écrit l'effet dans les **deux** entrées d'éclairage du profil, comme le profil d'usine (qui les a identiques), pour qu'il s'affiche quelle que soit celle que la souris utilise.
- **Mode économie d'énergie** : l'effet enregistré remplace aussi celui de la veille, à **mi-luminosité** comme dans les profils d'usine. Avant, la souris repassait à l'effet d'origine en veille.
- Format de mémoire de la G502 X PLUS confirmé par son diagnostic (taille, CRC, boutons, éclairage).

## Correctif
- **Identifiant à zéro** : la G502 X PLUS annonce un numéro de série vide (que des zéros) ; Batly l'utilisait comme identifiant, si bien que deux appareils dans ce cas se seraient confondus. Il utilise désormais l'identifiant matériel de l'appareil.
  ⚠️ Pour ces appareils, l'historique de batterie et les réglages propres à l'appareil (nom personnalisé, seuil…) repartent de zéro une fois.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.1.0-beta.2.exe` | Installateur |
| `Batly-5.1.0-beta.2-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
