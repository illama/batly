Batly 5.3.0 : une nouvelle allure, partout, et trois correctifs.

## Nouveau look
- **Une seule identité visuelle** pour le panneau, le menu, les fenêtres et la page web : fond graphite, accent vert, bleu pour la charge, ambre et rouge pour les niveaux bas. Thème clair et sombre suivant Windows.
- **Panneau des batteries** : formes lissées, une jauge par appareil qui se remplit à l'ouverture, apparition en cascade, survol en fondu, courbe qui se trace, curseur main sur tout ce qui se clique.
- **Menu de la zone de notification** redessiné : survol en pastille, coches vertes, raccourcis et niveaux alignés à droite.
- **Réglages et Récepteurs** : bouton principal vert, boutons arrondis, cases à cocher, boutons radio, listes déroulantes, champs de saisie et curseurs redessinés, avec effets de survol et de focus.
- **Page web** : en-tête avec logo, résumé (appareils, en charge, niveau le plus bas), **recherche** et **tri** (ordre habituel, niveau, nom, type), cartes avec jauge circulaire animée, courbe en dégradé, relief au survol. Les animations se coupent si Windows demande moins d'animations.

## Correctifs
- **Niveau figé pendant la charge** : certains appareils (PRO X 60) ne signalent que le début et la fin d'une charge ; un appareil en charge est maintenant relu chaque minute.
- **Casques Xbox fantômes** : Windows annonçait un casque à 100 % pour chaque emplacement de manette, même vide. Un casque n'est plus listé que s'il est branché sur une manette présente.
- **Panneau** : un clic sur un appareil déplie de nouveau sa courbe au lieu de fermer le panneau.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.3.0.exe` | Installateur (recommandé) |
| `Batly-5.3.0-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
| `SHA256SUMS.txt.minisig` | Signature des sommes (clé : `packaging/batly-minisign.pub`) |

Windows 10 (1607) ou plus récent, x64. Les installations 5.x proposeront la mise à jour automatiquement.
