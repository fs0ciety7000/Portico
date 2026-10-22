# Importer depuis Termius ou Termix

Portico lit un export de vos hôtes Termius ou Termix, montre ce qu'il va importer, puis en fait des **hôtes Portico**,
modifiables, dans son propre fichier de configuration SSH (`hosts.conf`, voir ADR « Hôtes créés dans Portico ») : ils
apparaissent aussitôt dans Portico ; pour les voir aussi avec `ssh`, `scp` ou `git`, ajoutez vous-même la ligne
`Include` proposée en tête de `~/.ssh/config` (Portico ne modifie jamais ce fichier). Les tags, groupes et favoris
vont dans `hosts.toml`. Aucun mot de passe n'est importé ; les clés privées d'un export Termix ne le sont que si vous
les cochez (voir « Termix » plus bas).

> **Attention** : un export Termius contient souvent vos **mots de passe et clés privées en clair**. Gardez-le hors de
> tout dossier synchronisé, et supprimez-le une fois l'import terminé (`shred -u export.csv` sous Linux, ou la corbeille
> vidée sous macOS).

## 1. Obtenir un fichier

Termius **ne propose pas d'export officiel des hôtes** dans l'application (vérifié en octobre 2026). Portico lit les
formats que l'on peut réellement produire, reconnus par leur contenu et non par leur nom :

| Source | Fichier | Remarques |
|---|---|---|
| Modèle d'import CSV de Termius, rempli à la main ou par un script | `.csv` dont l'en-tête est `Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,Password,SSH_KEY` | Ce format n'est **pas documenté officiellement** : Portico le lit avec tolérance (colonnes dans n'importe quel ordre, casse et ponctuation de l'en-tête ignorées, colonnes inconnues signalées, BOM UTF-8, fins de ligne CRLF, séparateur `;` des tableurs français, UTF-16 avec BOM). |
| `termius-local-export` (outil communautaire, licence MIT) | `hosts.json` ou `credentials.csv` | Lit la base locale de l'app sur votre machine. Les cellules préfixées d'une apostrophe anti-formule (`'=…`) sont rétablies. |
| Ancienne CLI officielle `termius` (archivée) | sortie de `termius export-ssh-config` | Les labels non assainis (`Host prod one`) deviennent un alias (`prod-one`). Les `identityfile ~/.termius/…` ne sont pas repris : ils sont listés. |

Portico ne lit **pas** la base locale chiffrée de l'application Termius.

## 2. Prévisualiser, puis importer

**TUI / ligne de commande**

```bash
portico import ~/Downloads/termius.csv                  # aperçu : rien n'est écrit (format reconnu au contenu)
portico import ~/Downloads/termius.csv --prefix         # aperçu, alias préfixés (termius- ou termix-)
portico import ~/Downloads/termius.csv --apply          # écrit dans hosts.conf, puis affiche la ligne Include
portico import termix ~/Downloads/termix.json           # Termix seulement (tout autre format est refusé)
portico import termius ~/Downloads/termius.csv          # forme historique, toujours acceptée
```

Avec un export Termix qui contient des clés privées, l'aperçu les liste (type, empreinte `SHA256:…`, hôtes qui
l'utilisent, chiffrée ou non, « already in ~/.ssh/… (reused) » si une clé locale a la même empreinte) et rappelle
comment les sélectionner. **Aucune clé n'est importée sans sélection explicite** :

```bash
portico import termix.json --key SHA256:fAtep8ghWl7Uqy2Puv62F3OqOuzAcVZ7KpQw2C21KOM          # aperçu, clé cochée
portico import termix.json --key SHA256:fAte… --key SHA256:6ktW… --apply                    # deux clés (option répétable)
portico import termix.json --all-keys --key-passphrase --apply                              # toutes, chiffrées
```

- `--key SHA256:…` (répétable, aussi `--key=SHA256:…`) choisit une clé par son empreinte ; une empreinte absente de
  l'export est refusée (`import_unknown_key`), rien n'est écrit.
- `--all-keys` choisit toutes les clés de l'export.
- `--key-passphrase` demande, **sur le terminal, sans écho, deux fois**, une passphrase qui chiffre les copies des
  clés qui ne sont pas déjà chiffrées dans l'export. Elle n'est **jamais** acceptée comme valeur
  (`--key-passphrase=…` est refusé : les arguments sont visibles dans `ps`), ne vit qu'en mémoire (`Zeroizing`,
  `SecretString`), et n'est demandée que si une clé sélectionnée doit être écrite. `Ctrl-C` ou `Esc` arrête sans
  rien écrire. Sans elle, les clés en clair sont écrites sans passphrase (`0600`).

Le rapport liste les hôtes mis à jour, les clés importées (`~/.ssh/portico/<alias>`), les clés réutilisées, le
nombre de clés ignorées, les avertissements apparus à l'écriture, puis rappelle de **supprimer l'export**.

**GUI** : palette (`⌘K` / `Ctrl-K`), « Import from Termius… ». Le fichier se choisit dans le sélecteur natif ;
l'aperçu liste les hôtes, les enregistrements ignorés, les avertissements et les secrets ignorés. « Import » écrit,
puis la ligne `Include` s'affiche avec un bouton de copie. La TUI n'a pas d'écran d'import : son aide `?` rappelle la
commande ci-dessus.

Un nouvel import **met à jour** les hôtes que le précédent a créés et que personne n'a modifiés : l'aperçu les compte
(« Updates N hosts from an earlier import », `replaces`) et les marque `(update)`. Un export dont aucun hôte n'est importable n'est jamais appliqué (`--apply` refuse, le bouton est
désactivé) : les hôtes déjà importés restent en place.

## 3. Ajouter la ligne `Include`

Le rapport donne la ligne exacte à placer **en tête** de `~/.ssh/config`, avant tout bloc `Host` ou `Match` (placée
après un bloc, elle ne s'appliquerait qu'à ce bloc) :

```sshconfig
# Linux
Include ~/.config/portico/hosts.conf
# macOS (chemin avec un espace : entre guillemets)
Include "~/Library/Application Support/dev.Portico.portico/hosts.conf"
```

Portico affiche les hôtes importés sans cette ligne ; avec elle, OpenSSH (`ssh`, `scp`, `rsync`…) voit les mêmes
hôtes.

## Ce qui est importé, et ce qui ne l'est pas

| Donnée | Devient |
|---|---|
| Label | **Alias** : minuscules, accents translittérés, espaces et séparateurs en `-`, seulement `[a-z0-9_.-]` (64 caractères au plus). Deux labels identiques donnent `web-eu-1`, `web-eu-1-2`… Un alias qui ressemble à un nom d'hôte (il contient `.` ou `:`) mais ne correspond pas au `HostName` prend le préfixe `termius-`, avec un avertissement : le label `github.com` vers `203.0.113.66` devient `termius-github.com`, sinon `ssh github.com` partirait vers cette adresse. Si l'alias diffère du label, les notes de l'hôte gardent `Termius: <label>`. |
| Hostname/IP | `HostName` (obligatoire). IPv6 accepté, crochets retirés. |
| Username | `User` |
| Port | `Port`, omis s'il vaut 22 ; un port hors 1–65535 est omis avec un avertissement. |
| Groups | Groupe Portico (`Production/EU`, 64 caractères au plus), seulement si l'hôte n'en a pas déjà un. |
| Tags | Tags Portico, ajoutés à ceux qui existent (16 au plus) ; `Web Serveur` devient `web-serveur` avec un avertissement. |
| Protocol | Seul `ssh` (ou vide) est importé : telnet, mosh… sont ignorés. |
| Password, SSH_KEY, PrivateKeyPath, identityFile | **Jamais importés**. Portico compte seulement les cellules non vides (« 2 mots de passe et 1 clé ignorés ») et les efface de la mémoire. Une clé privée collée dans une autre colonne est aussi détectée et ignorée. |

Les hôtes importés rejoignent `hosts.conf` (dossier de configuration de Portico, `0600`, dossier `0700`), avec
seulement `Host`, `HostName`, `User`, `Port` et, pour une clé importée, `IdentityFile` et `IdentitiesOnly yes`.
`hosts.toml` garde leur **provenance** (`[hosts.<alias>.import]` : source, identifiant de l'enregistrement, empreinte
des valeurs écrites). **Réimporter** met à jour les hôtes que l'import a créés et que personne n'a modifiés depuis ;
un hôte modifié à la main (valeurs différentes de l'empreinte), créé à la main, ou défini ailleurs n'est jamais
touché : l'enregistrement est ignoré (`edited_since_import`, `alias_collision`). Modifier seulement les tags, le
groupe ou le favori n'empêche pas les mises à jour. Un réimport ne supprime aucun hôte ; réimporter le même export ne
modifie rien.

Un ancien fichier `imports/termius.conf` (premières versions de l'import) reste lu tel quel, en lecture seule ; ses
alias ne sont jamais masqués par un nouvel import.

**Sécurité** : chaque valeur est vérifiée avant d'être écrite (`HostName` : lettres, chiffres, `.`, `-`, `_`, `:` ;
`User` : lettres, chiffres, `.`, `-`, `_`, `@`, `+` ; ni espace, ni saut de ligne, ni `#`, ni guillemet, ni `%`). Un
enregistrement refusé est **ignoré en entier**, jamais importé à moitié. Une ligne CSV qui n'a pas le même nombre de
cellules que l'en-tête est ignorée dès qu'une colonne contient un secret (une ligne décalée pourrait placer un mot de
passe dans `User`) ; sans colonne secrète, seules les cellules manquantes en fin de ligne sont tolérées : un export piégé ne peut pas injecter
`ProxyCommand` ou une autre directive. Portico n'écrit jamais `ProxyCommand` ni `StrictHostKeyChecking`, et
`IdentityFile` seulement vers une clé que vous avez sélectionnée (Termix).

**Collisions** : un alias déjà défini dans votre configuration (sans tenir compte de la casse) n'est pas importé,
avec un avertissement, pour ne pas masquer votre propre entrée. L'option `--prefix` (case « Prefix every alias with
termius- » dans la GUI) préfixe tous les alias importés et garde les deux.

Limites connues : ni `ProxyJump`, ni clés, ni snippets, ni identités partagées Termius (formats non confirmés) ;
5000 hôtes et 4 Mio par fichier au plus ; 500 avertissements listés au plus (les suivants sont comptés).

## Termix

Un export JSON de Termix (`{"hosts":[{"name","ip","port","username","authType","password","key","keyPassword",
"folder","tags","pin",…}]}`) est reconnu par son contenu (des entrées avec `ip`). `name` devient l'alias (mêmes règles
que les labels Termius, préfixe `termix-`), `ip` le `HostName`, `username` le `User`, `port` le `Port` (absent : 22),
`folder` le groupe, `tags` les tags et `pin` le favori (posé à la création seulement). `tunnelConnections` et
`socks5ProxyChain` ne sont pas importés : l'aperçu les signale. `password` et `keyPassword` ne sont **jamais**
importés (comptés).

**Clés privées** : l'aperçu liste chaque clé distincte (type, empreinte `SHA256:…`, hôtes qui l'utilisent, chiffrée
ou non, copie locale déjà présente) ; aucune n'est cochée. Formats lus : OpenSSH (en clair ou chiffrée) et PEM
(`RSA PRIVATE KEY` PKCS#1, `EC PRIVATE KEY` SEC1, `PRIVATE KEY` PKCS#8 : RSA, ECDSA, Ed25519 ; une PEM chiffrée
seulement si l'export contient son `keyPassword`, utilisé en mémoire pour lire l'empreinte puis effacé). Une clé
PuTTY, DSA ou illisible est ignorée avec un avertissement qui ne la cite jamais. Une clé cochée dont une clé locale a
la même empreinte (`~/.ssh`, `~/.ssh/portico`) est **réutilisée** ; sinon elle est écrite dans
`~/.ssh/portico/<alias>` (`0600`, dossier `0700`, jamais d'écrasement : `-2`, `-3`… si le nom est pris) avec son
`.pub`, au format OpenSSH, chiffrée par la passphrase que vous donnez ; une clé déjà chiffrée dans l'export est
écrite telle quelle (sa passphrase ne change pas). Les hôtes qui l'utilisent reçoivent `IdentityFile` et
`IdentitiesOnly yes` ; sans clé cochée, aucun `IdentityFile` (un réimport garde celle qu'un hôte a déjà). Le rapport
compte les clés importées, réutilisées et ignorées, et rappelle de **supprimer l'export**.
