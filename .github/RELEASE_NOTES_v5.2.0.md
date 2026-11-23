Batly 5.2.0 : mises à jour signées, API qui agit, fenêtres sombres et courbe dans le panneau.

## Nouveau
- **Courbe dans le panneau** : un clic sur un appareil déplie sa batterie sur **7 ou 30 jours**, avec l'autonomie d'une charge, les cycles et la santé. Le bouton *Réglages* de la courbe ouvre les réglages comme avant.
- **L'API peut modifier les appareils** : `POST /api/device/{id}/settings` règle le DPI, le taux de rapport, le rétroéclairage, SmartShift, l'éclairage (aperçu, enregistrement, garde) et le **canal Easy-Switch**. Le Stream Deck, Home Assistant ou un script peuvent maintenant agir, pas seulement afficher. Toujours protégé par le **jeton** (en-tête `Authorization: Bearer`) : sans jeton défini, l'API reste en lecture seule.
- **Thème sombre** pour les fenêtres *Réglages* et *Récepteurs et appareils*, et pour le menu de la zone de notification. Barre de titre **Mica** et coins arrondis sous Windows 11.
- **Premier lancement guidé** : quels périphériques surveiller, une icône par appareil ou une seule, quelles alertes, démarrage avec Windows.

## Améliorations
- **Récepteurs et appareils** ne dépasse plus de l'écran : la fenêtre se règle sur la hauteur disponible, se redimensionne et défile (molette, barre, touche Tab).
- **Moins de réveils radio** : un appareil qui signale lui-même ses changements de batterie est relu 4 fois moins souvent, et au plus une fois par heure quand il se dit inactif.

## Sécurité
- **Mises à jour signées** : la liste `SHA256SUMS.txt` est signée avec minisign (`SHA256SUMS.txt.minisig`) et vérifiée avec la clé publique intégrée à Batly. Un fichier remplacé sur GitHub est refusé, même si quelqu'un prend la main sur le compte.
- **Attestation de provenance** : chaque fichier publié peut être vérifié avec `gh attestation verify <fichier> --repo illama/batly`.
- Dépendances suivies chaque semaine (Dependabot) et auditées (`cargo audit`) en intégration continue.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.2.0.exe` | Installateur (recommandé) |
| `Batly-5.2.0-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
| `SHA256SUMS.txt.minisig` | Signature des sommes (clé : `packaging/batly-minisign.pub`) |

Windows 10 (1607) ou plus récent, x64. Les installations 5.x proposeront la mise à jour automatiquement.
