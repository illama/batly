# Signature du code (Authenticode)

## Pourquoi signer

Windows marque tout fichier téléchargé (« Mark of the Web »). Au lancement d'un exécutable
**non signé** et peu téléchargé, **SmartScreen** affiche « Windows a protégé votre ordinateur »
et cache le bouton *Exécuter quand même* derrière *Informations complémentaires* : beaucoup
d'utilisateurs s'arrêtent là. Les antivirus sont aussi plus soupçonneux envers un petit
exécutable inconnu qui lit des périphériques HID et ouvre un port HTTP local.

Une signature Authenticode :

- affiche l'éditeur au lieu de « Éditeur inconnu » dans les fenêtres de Windows ;
- permet de bâtir une **réputation SmartScreen** attachée au certificat (et non à chaque
  fichier) : l'avertissement disparaît après un certain volume de téléchargements ;
- prouve que l'exécutable n'a pas été modifié depuis la compilation (la mise à jour intégrée
  vérifie déjà la somme SHA-256 de `SHA256SUMS.txt`, la signature s'y ajoute) ;
- facilite l'acceptation dans winget et Chocolatey (moins de faux positifs).

## Ce qui est déjà automatisé

`.github/workflows/release.yml`, déclenché par un tag `v<version>` :

1. tests, compilation MSVC de `batly.exe` ;
2. **si** les secrets `SIGNPATH_API_TOKEN` et `SIGNPATH_ORGANIZATION_ID` existent :
   envoi de `batly.exe` à SignPath (action officielle
   `signpath/github-action-submit-signing-request`), attente de la signature ;
3. compilation de l'installateur **avec l'exécutable signé embarqué** ;
4. signature de l'installateur par SignPath ;
5. zip portable (avec l'exécutable signé), `SHA256SUMS.txt`, contrôle
   `Get-AuthenticodeSignature` des deux fichiers ;
6. release GitHub **publiée directement**, en préversion pour un tag `v…-…` (notes : `.github/RELEASE_NOTES_<tag>.md` si présent) ;
7. manifestes winget / Scoop / Chocolatey en artefact.

Sans les secrets, les étapes 2, 4 et le contrôle de signature sont sautées : la release est
produite non signée, comme aujourd'hui.

## Signature des mises à jour (minisign) — toujours active

Indépendamment d'Authenticode, la liste `SHA256SUMS.txt` de chaque version est signée avec
[minisign](https://jedisct1.github.io/minisign/) (`SHA256SUMS.txt.minisig`). Batly embarque la
clé publique (`packaging/batly-minisign.pub`, constante `PUBLIC_KEY` de `app/src/update.rs`) et
**refuse d'installer** une mise à jour dont la liste n'est pas signée par cette clé. Prendre la
main sur le compte GitHub ou remplacer les fichiers d'une release ne suffit donc plus à
pousser un exécutable aux utilisateurs.

- La clé secrète n'est **jamais** dans le dépôt : elle est dans le secret GitHub
  `MINISIGN_SECRET_KEY` (contenu complet du fichier `.key`, sans mot de passe) et chez le
  mainteneur. Sans ce secret, le workflow de release **échoue** au lieu de publier une version
  que la mise à jour automatique refuserait.
- Vérifier une version à la main :
  ```sh
  minisign -Vm SHA256SUMS.txt -p packaging/batly-minisign.pub
  sha256sum --check SHA256SUMS.txt
  ```
- Chaque fichier publié a aussi une **attestation de provenance** GitHub (il sort bien du
  workflow de ce dépôt, à ce commit) :
  ```sh
  gh attestation verify Batly-Setup-<version>.exe --repo illama/batly
  ```
- Changer de clé : générer une nouvelle paire (`minisign -G -W`), publier d'abord une version
  signée par l'**ancienne** clé qui embarque la nouvelle clé publique, puis seulement remplacer
  le secret.

## SignPath Foundation (gratuit pour l'open source)

[SignPath Foundation](https://signpath.org) fournit gratuitement un certificat de signature
aux projets open source. Le certificat est émis **au nom de SignPath Foundation** (l'éditeur
affiché est « SignPath Foundation »), la clé reste dans leur HSM, et chaque signature est
liée à une compilation vérifiable sur GitHub Actions.

### Conditions (résumé, voir [signpath.org/terms](https://signpath.org/terms))

- licence open source reconnue par l'OSI, **sans** composant propriétaire ni commercial
  (GPL-3.0 : OK) ;
- projet maintenu et **déjà publié** (au moins une release), avec une page qui décrit
  l'application ;
- les fichiers signés sont compilés à partir du dépôt public, par le CI (GitHub Actions),
  jamais sur une machine personnelle ;
- pas de logiciel malveillant ni « potentiellement indésirable » ; l'application ne doit pas
  modifier le système sans le consentement de l'utilisateur (la mise à jour automatique doit
  rester un choix de l'utilisateur : c'est le cas, elle se lance depuis le menu) ;
- une **politique de signature du code** publiée dans le dépôt (qui peut demander la
  signature, qui approuve) et l'authentification à deux facteurs sur les comptes concernés ;
- mention de SignPath dans le README (« Free code signing provided by SignPath.io,
  certificate by SignPath Foundation »).

### Étapes

1. **Préparer le dépôt** :
   - ajouter au README une section *Code signing policy* : « Free code signing provided by
     [SignPath.io](https://about.signpath.io), certificate by
     [SignPath Foundation](https://signpath.org) », les rôles (auteurs/relecteurs :
     membres du dépôt ; approbateurs : `illama`) et la phrase de confidentialité
     (« This program will not transfer any information to other networked systems unless
     specifically requested by the user or the person installing or operating it. ») —
     à nuancer pour Batly : la vérification des mises à jour interroge l'API GitHub une
     fois par jour, désactivable par `[Updates] check = false` ;
   - activer la 2FA sur le compte GitHub ;
   - avoir au moins une release publiée.
2. **Candidater** : formulaire sur [signpath.org/apply](https://signpath.org/apply)
   (nom du projet, dépôt, licence, description, nombre d'utilisateurs, mainteneurs).
   La réponse prend en général de quelques jours à quelques semaines.
3. **Une fois accepté**, SignPath crée une organisation sur
   [app.signpath.io](https://app.signpath.io). Y configurer :
   - **Trusted build system** : *GitHub.com*, relié au dépôt
     `illama/batly` (installer l'application GitHub SignPath si demandé) ;
   - **Projet** : slug `batly`, dépôt lié ;
   - **Artifact configurations** (l'artefact GitHub est un zip contenant le fichier) :
     - slug `exe` :
       ```xml
       <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
         <zip-file>
           <pe-file path="batly.exe">
             <authenticode-sign />
           </pe-file>
         </zip-file>
       </artifact-configuration>
       ```
     - slug `installer` :
       ```xml
       <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
         <zip-file>
           <pe-file path="Batly-Setup-*.exe">
             <authenticode-sign />
           </pe-file>
         </zip-file>
       </artifact-configuration>
       ```
   - **Signing policy** : slug `release-signing`, certificat *release* de la fondation,
     approbation manuelle (un clic dans SignPath ou par e-mail à chaque release), origine
     vérifiée : dépôt, branche/tag `refs/tags/v*`, workflow `release.yml` ;
   - un **utilisateur CI** (*Users → New CI user*) : copier son **jeton d'API**.
4. **Secrets GitHub** (*Settings → Secrets and variables → Actions*) :

   | Type | Nom | Valeur |
   |---|---|---|
   | Secret | `SIGNPATH_API_TOKEN` | jeton de l'utilisateur CI |
   | Secret | `SIGNPATH_ORGANIZATION_ID` | identifiant (GUID) de l'organisation SignPath |
   | Variable (facultatif) | `SIGNPATH_PROJECT_SLUG` | si différent de `batly` |
   | Variable (facultatif) | `SIGNPATH_POLICY_SLUG` | si différent de `release-signing` |
   | Variable (facultatif) | `SIGNPATH_EXE_CONFIG` / `SIGNPATH_SETUP_CONFIG` | si différents de `exe` / `installer` |

5. **Tester** : pousser un tag (par ex. `v4.1.0`), approuver les deux demandes dans
   SignPath, vérifier sous Windows : *Propriétés → Signatures numériques*, ou
   `Get-AuthenticodeSignature .\Batly-Setup-4.1.0.exe`.

La réputation SmartScreen se construit ensuite avec les téléchargements : l'avertissement
peut subsister quelques semaines après les premières versions signées.

## Alternatives

| Solution | Coût | Remarques |
|---|---|---|
| **Azure Artifact Signing** (ex-*Trusted Signing*) | ~10 $/mois | Certificat au nom du développeur après vérification d'identité ; réservé aux entreprises et aux particuliers de certains pays (vérifier l'éligibilité). Action GitHub `azure/trusted-signing-action`. Réputation SmartScreen rapide. |
| **Certificat OV** (Sectigo, SSL.com, Certum…) | 100–400 €/an | Depuis 2023 la clé doit être sur un jeton matériel ou un HSM cloud (ex. SSL.com eSigner, utilisable en CI). Certum propose une offre « Open Source Code Signing » moins chère pour les particuliers. |
| **Certificat EV** | 300–600 €/an | Ne donne plus de réputation SmartScreen immédiate depuis 2024 : peu d'intérêt de plus qu'un OV ici. |
| Ne pas signer | 0 | Avertissement SmartScreen ; documenter *Informations complémentaires → Exécuter quand même* et publier `SHA256SUMS.txt` (déjà fait). |

Pour brancher une autre solution, remplacer les deux étapes « Sign … (SignPath) » du
workflow en gardant l'ordre : **signer `batly.exe` avant de compiler l'installateur**, puis
signer l'installateur.
