# Plugin Stream Deck Batly

Une action **Battery level** pour Elgato Stream Deck : la touche affiche la batterie d'un
périphérique (nom, jauge colorée, pourcentage, éclair pendant la charge ; grisée s'il est
hors ligne). Mise à jour en direct par le flux d'événements de Batly (`/api/events`), avec
repli automatique sur un sondage de `/api/devices` toutes les 30 secondes.

- **Appui court** : actualise immédiatement.
- **Appui long** (plus de 0,6 s) : ouvre la page web de Batly.

Le plugin est en HTML/JavaScript (SDK v2) : rien à compiler.

## Installation

1. Stream Deck 6.0 ou plus récent, sous Windows 10/11.
2. Vérifier que <http://localhost:12321/api/devices> répond (serveur HTTP de Batly actif).
3. Fermer complètement l'application Stream Deck (icône de notification → *Quitter*).
4. Copier le dossier `com.illama.batly.sdPlugin` dans
   `%APPDATA%\Elgato\StreamDeck\Plugins\`.
5. Relancer Stream Deck : la catégorie **Batly** apparaît dans la liste des actions.

Pour en faire un fichier installable par double-clic, utiliser l'outil officiel d'Elgato :
`streamdeck pack com.illama.batly.sdPlugin` (CLI `@elgato/cli`) ou l'ancien
`DistributionTool.exe -b -i com.illama.batly.sdPlugin -o .`, qui produit
`com.illama.batly.streamDeckPlugin`.

## Configuration

Glisser **Battery level** sur une touche, puis dans le panneau du bas :

| Champ | Rôle |
|---|---|
| Batly | adresse de Batly (vide = `http://localhost:12321`) ; un autre PC : `http://nom-ou-ip:12321` |
| Token | jeton d'accès, seulement pour un autre PC qui en a défini un |
| Device | périphérique à afficher ; *Lowest battery (automatic)* montre celui qui a le niveau le plus bas |
| Show name | affiche le nom (ou l'alias défini dans Batly) en haut de la touche |

La liste des périphériques est lue dans `/api/devices` (bouton *Reload device list* pour la
relire). Un périphérique déconnecté reste choisi et s'affichera à son retour.

## Dépannage

- Touche avec « ? » : périphérique introuvable, ou Batly injoignable (message dans le panneau).
- Pour un autre PC : `addr = "0.0.0.0"` dans `[HTTPServer]` sur ce PC, port ouvert dans le
  pare-feu, et le même jeton des deux côtés.
- Journal du plugin : activer le débogage HTML de Stream Deck puis ouvrir
  <http://localhost:23654/>.
