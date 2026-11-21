Batly 5.1.0-beta.4 : l'éclairage des claviers gardé par Batly.

> **Préversion expérimentale**, non proposée par la mise à jour automatique.

## Éclairage
- **Claviers et appareils sans mémoire inscriptible** (PRO X 60…) : *Enregistrer dans l'appareil…* fait **garder l'éclairage par Batly**. Il est appliqué tout de suite, puis à chaque connexion, reconnexion ou réveil de l'appareil, tant que Batly tourne sur ce PC (le PRO X 60 n'a aucune mémoire où l'enregistrer lui-même).
  - Quand l'appareil devient inactif, ses LEDs s'éteignent en fondu comme d'habitude, puis se rallument avec votre couleur à la première touche.
  - À la fermeture de Batly, les LEDs sont rendues à l'appareil, qui reprend son propre éclairage.
  - *Restaurer…* arrête de garder l'éclairage.
- La souris G502 X PLUS garde, elle, l'éclairage dans sa propre mémoire (beta.3).

## Correctif
- **Réponses mélangées** : une réponse arrivée en retard d'un périphérique pouvait être prise pour la réponse à la question suivante (fonctions mal identifiées, vu dans le diagnostic du PRO X 60). Chaque requête porte maintenant son propre numéro.

## Diagnostic
- Ajout de la gestion de profils des claviers récents (0x8101), en lecture seule.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.1.0-beta.4.exe` | Installateur |
| `Batly-5.1.0-beta.4-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
