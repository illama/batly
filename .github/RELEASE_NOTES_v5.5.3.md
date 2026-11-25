Batly 5.5.3 : mémoire embarquée respectée, Home Assistant corrigé.

## Correctifs
- **Mémoire embarquée des souris (G HUB « mémoire intégrée »)** : un éclairage « gardé par Batly » enregistré sur une souris à mémoire embarquée (ex. G502 X PLUS) était réappliqué à chaque connexion, ce qui sortait la souris de sa mémoire. Batly ne garde plus jamais l'éclairage d'un appareil à mémoire embarquée (et ignore un ancien réglage de ce type) : la souris reste sur sa mémoire. L'éclairage gardé reste disponible pour les claviers sans mémoire (PRO X 60).
- **Home Assistant** : les appareils n'apparaissaient pas, Home Assistant refusant les déclarations contenant un champ vide (« suggested_area »). Les champs vides ne sont plus envoyés. La lumière n'est plus proposée pour les appareils à mémoire embarquée.
- **G HUB** : Batly le considère aussi comme ouvert dès qu'il est connecté à son service (en plus de ses processus).

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.5.3.exe` | Installateur (recommandé) |
| `Batly-5.5.3-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
| `SHA256SUMS.txt.minisig` | Signature des sommes (clé : `packaging/batly-minisign.pub`) |

Windows 10 (1607) ou plus récent, x64. Les installations 5.x proposeront la mise à jour automatiquement.
