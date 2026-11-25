Batly 5.4.0 : des fenêtres entièrement redessinées et animées.

## Nouveau
- **Réglages, Récepteurs et appareils, Bienvenue** refaits de zéro dans le style Batly : cartes arrondies, police Segoe UI, onglets dont le soulignement glisse, pages en fondu, **interrupteurs animés**, listes déroulantes avec chevron, survols et sélections animés, curseur main sur tout ce qui se clique. Toujours une application Rust, sans navigateur ni carte graphique requise.
- **Menu du clic droit** remplacé par un menu Batly : il glisse à l'ouverture, survol en pastille animée, icône et niveau en couleur de chaque appareil, navigation au clavier (flèches, Entrée, Échap).
- **Récepteurs et appareils** : confirmations dans la fenêtre, sélecteur de couleur intégré, réglages non gérés par l'appareil masqués, réglage envoyé au relâchement d'un curseur.
- **Page web** : liste de tri faite maison et animée (coche sur le choix actif, clavier).

## Correctifs
- **Casque Xbox fantôme** : les casques ne sont plus lus via XInput (un casque branché sur une manette n'a pas de batterie propre ; les pilotes renvoyaient le niveau de la manette).

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.4.0.exe` | Installateur (recommandé) |
| `Batly-5.4.0-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
| `SHA256SUMS.txt.minisig` | Signature des sommes (clé : `packaging/batly-minisign.pub`) |

Windows 10 (1607) ou plus récent, x64. Les installations 5.x proposeront la mise à jour automatiquement.
