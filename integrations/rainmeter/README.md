# Skin Rainmeter LGSTray

Affiche sur le bureau le niveau de batterie de quatre périphériques au plus, lu dans l'API
locale de LGSTray (`/api/devices`) : nom, pourcentage, jauge (rouge sous 16 %) et ⚡ pendant
la charge. Clic sur le titre (ou clic droit → *Open LGSTray page*) : ouvre la page de LGSTray.

## Installation

1. Installer [Rainmeter](https://www.rainmeter.net/) (4.5 ou plus récent).
2. Vérifier que le serveur HTTP de LGSTray est actif (`[HTTPServer] enabled = true`, c'est le
   réglage par défaut) : <http://localhost:12321/api/devices> doit répondre.
3. Copier le dossier `LGSTray` dans `Documents\Rainmeter\Skins\`.
4. Rainmeter → *Gérer* → *Actualiser tout*, puis charger `LGSTray\LGSTray.ini`.

## Configuration

Clic droit sur le skin → *Modifier le skin*, section `[Variables]` :

| Variable | Rôle | Défaut |
|---|---|---|
| `Url` | adresse de LGSTray ; pour un autre PC : `http://nom-ou-ip:12321` | `http://localhost:12321` |
| `Token` | jeton d'accès, seulement si l'autre PC en a défini un (`[HTTPServer] token`) | vide |
| `Refresh` | intervalle de rafraîchissement, en secondes | `30` |
| `Width`, `FontFace`, `FontSize` | taille et police | `220`, `Segoe UI`, `10` |
| `TextColor`, `BarColor`, `LowColor`, `Background`… | couleurs (R,G,B,A) | thème sombre |

Enregistrer puis *Actualiser le skin*.

Les quatre premiers périphériques de la liste sont affichés (ordre alphabétique de LGSTray) ;
les lignes vides sont masquées. Pour en afficher plus, dupliquer un bloc `device 4` en
renommant les mesures/compteurs et en ajoutant un `#Item#` à `RegExp`.

Une instance LGSTray qui agrège d'autres PC (réglage `peers`) les montre aussi : leurs
périphériques apparaissent avec le nom de l'ordinateur dans la page web.

## Dépannage

- « LGSTray: no connection » : LGSTray ne tourne pas, ou le serveur HTTP est désactivé, ou
  l'adresse/le port diffère (`[HTTPServer] port`).
- Rien ne s'affiche depuis un autre PC : `addr` doit valoir `0.0.0.0` sur la machine qui expose
  ses périphériques, le pare-feu doit autoriser le port, et `Token` doit correspondre.
