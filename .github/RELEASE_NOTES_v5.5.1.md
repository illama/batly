Batly 5.5.1 : correctif important avec G HUB, et page web plus lisible.

## Correctif important
- **Touches G HUB désassignées** : en rendant la main sur l'éclairage (bouton « Rendre la main », fermeture de Batly, éclairage gardé qui se met en veille), Batly remettait toujours la souris ou le clavier en « mémoire embarquée ». G HUB, qui garde ses appareils en mode hôte pour appliquer vos assignations de touches, ne pouvait plus les appliquer. Batly remet maintenant exactement le mode d'origine.
- **G HUB ouvert = Batly ne touche plus à l'éclairage** : tant que G HUB tourne, Batly ne change ni l'éclairage ni le mode des appareils (et rend la main s'il l'avait). La batterie et les autres réglages restent lus normalement.

Si vos touches G HUB ne fonctionnent plus : dans G HUB, ouvrez l'appareil et désactivez « Mode mémoire intégrée » (ou utilisez le bouton du bandeau).

## Page web
- Les boutons des cartes ouvrent de vraies vues au lieu des données brutes :
  - **Historique** : grande courbe sur 24 h, 7 ou 30 jours, statistiques (autonomie, durée de charge, consommation, santé, cycles) ;
  - **Détails** : tout ce que Batly sait de l'appareil (modèle, micrologiciel, série, récepteur, canal Easy-Switch, signal…) ;
  - **Overlay** : aperçu en direct, thème et taille, lien à copier pour OBS.
- Lien direct vers une vue : `#history=<id>`, `#details=<id>`, `#overlay=<id>`. Le JSON et le XML restent accessibles depuis « Détails ».

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.5.1.exe` | Installateur (recommandé) |
| `Batly-5.5.1-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
| `SHA256SUMS.txt.minisig` | Signature des sommes (clé : `packaging/batly-minisign.pub`) |

Windows 10 (1607) ou plus récent, x64. Les installations 5.x proposeront la mise à jour automatiquement.
