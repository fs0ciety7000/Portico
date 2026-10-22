# Importer depuis Termius

Portico lit un export de vos hôtes Termius, montre ce qu'il va importer, puis écrit ces hôtes dans **son propre
fichier** de configuration SSH. Vous ajoutez vous-même une ligne `Include` en tête de `~/.ssh/config` : Portico ne
modifie jamais ce fichier. Les tags et les groupes vont dans `hosts.toml`. Aucun secret n'est importé.

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
portico import termius ~/Downloads/termius.csv            # aperçu : rien n'est écrit
portico import termius ~/Downloads/termius.csv --prefix   # aperçu, alias préfixés par termius-
portico import termius ~/Downloads/termius.csv --apply    # écrit, puis affiche la ligne Include
```

**GUI** : palette (`⌘K` / `Ctrl-K`), « Import from Termius… ». Le fichier se choisit dans le sélecteur natif ;
l'aperçu liste les hôtes, les enregistrements ignorés, les avertissements et les secrets ignorés. « Import » écrit,
puis la ligne `Include` s'affiche avec un bouton de copie. La TUI n'a pas d'écran d'import : son aide `?` rappelle la
commande ci-dessus.

Un nouvel import **remplace** les hôtes du précédent : l'aperçu l'annonce (« Replaces the 6 hosts of … », bouton
« Replace with N hosts »). Un export dont aucun hôte n'est importable n'est jamais appliqué (`--apply` refuse avec
« nothing to import: termius.conf left as is », le bouton est désactivé) : l'import précédent reste en place.

## 3. Ajouter la ligne `Include`

Le rapport donne la ligne exacte à placer **en tête** de `~/.ssh/config`, avant tout bloc `Host` ou `Match` (placée
après un bloc, elle ne s'appliquerait qu'à ce bloc) :

```sshconfig
# Linux
Include ~/.config/portico/imports/termius.conf
# macOS (chemin avec un espace : entre guillemets)
Include "~/Library/Application Support/dev.Portico.portico/imports/termius.conf"
```

Rechargez ensuite la liste des hôtes (`r` dans la TUI, `⌘R` / `Ctrl-R` dans la GUI). OpenSSH (`ssh`, `scp`, `rsync`…)
voit les mêmes hôtes.

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

Le fichier `imports/termius.conf` (dossier de configuration de Portico, `0600`, dossier `0700`) ne contient que des
directives `Host`, `HostName`, `User` et `Port`. Il est **remplacé en entier** à chaque import et n'est réécrit que si
un hôte change : réimporter le même export ne modifie rien.

**Modifier un hôte importé** : copiez son bloc dans `~/.ssh/config`, puis **réimportez**. Tant que vous ne réimportez
pas, la copie placée après la ligne `Include` est masquée (OpenSSH garde la première valeur trouvée, celle de
`termius.conf`). Au réimport, Portico voit votre copie et retire cet hôte de `termius.conf`.

**Sécurité** : chaque valeur est vérifiée avant d'être écrite (`HostName` : lettres, chiffres, `.`, `-`, `_`, `:` ;
`User` : lettres, chiffres, `.`, `-`, `_`, `@`, `+` ; ni espace, ni saut de ligne, ni `#`, ni guillemet, ni `%`). Un
enregistrement refusé est **ignoré en entier**, jamais importé à moitié. Une ligne CSV qui n'a pas le même nombre de
cellules que l'en-tête est ignorée dès qu'une colonne contient un secret (une ligne décalée pourrait placer un mot de
passe dans `User`) ; sans colonne secrète, seules les cellules manquantes en fin de ligne sont tolérées : un export piégé ne peut pas injecter
`ProxyCommand` ou une autre directive. Portico n'écrit jamais `ProxyCommand`, `StrictHostKeyChecking` ni
`IdentityFile`.

**Collisions** : un alias déjà défini dans votre configuration (sans tenir compte de la casse) n'est pas importé,
avec un avertissement, pour ne pas masquer votre propre entrée. L'option `--prefix` (case « Prefix every alias with
termius- » dans la GUI) préfixe tous les alias importés et garde les deux.

Limites connues : ni `ProxyJump`, ni clés, ni snippets, ni identités partagées Termius (formats non confirmés) ;
5000 hôtes et 4 Mio par fichier au plus ; 500 avertissements listés au plus (les suivants sont comptés).
