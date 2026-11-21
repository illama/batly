Batly 5.0.1 : une API mieux protégée et une base de code plus solide.

## Sécurité
- **API HTTP fermée aux sites web** : jusqu'ici, n'importe quelle page ouverte dans le navigateur pouvait lire la liste de vos périphériques et leur historique sur `http://localhost:12321`. Désormais, seules les pages servies depuis votre ordinateur (`localhost`, `127.0.0.1`) y ont accès. L'overlay OBS, la page web et Rainmeter ne sont pas concernés.
- **Protection contre le DNS rebinding** : sans jeton, l'API n'accepte que les requêtes adressées à cet ordinateur (`localhost`, une adresse IP ou le nom du PC, éventuellement suivi d'un domaine local comme `pc-salon.local`).
  ⚠️ Si un autre PC joint Batly par un **alias DNS** différent du nom de l'ordinateur, définissez un jeton (*Réglages → Réseau*) et ajoutez-le à l'adresse (`http://alias:12321#jeton`).

## Correctifs
- L'historique ne peut plus faire fermer l'application sur une valeur de mesure invalide.
- Nettoyage du code (avertissements, code inutilisé) et vérifications automatiques renforcées (formatage, analyse statique, traductions complètes).

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.0.1.exe` | Installateur (recommandé) |
| `Batly-5.0.1-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |

Windows 10 (1607) ou plus récent, x64. Batly 5.0.0 proposera la mise à jour automatiquement.
