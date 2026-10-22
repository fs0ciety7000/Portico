# Sécurité : Portico

## Signaler une vulnérabilité

Merci de **ne pas** ouvrir d'issue publique. Utilisez le signalement privé de GitHub
(onglet *Security* du dépôt → *Report a vulnerability*). Indiquez la version (`portico --version`),
la plateforme, et si possible un scénario reproductible. Nous accusons réception sous 7 jours et
publions un correctif puis un avis (GitHub Security Advisory) une fois les utilisateurs protégés.

## Ce que Portico garantit

- **Local-first** : aucun compte, aucun service distant, aucune télémétrie.
- **`~/.ssh/config` n'est jamais écrit.** Les seules écritures dans `~/.ssh` sont l'ajout dans
  `known_hosts` après accord explicite et la génération de clés que vous demandez (jamais d'écrasement).
- **Clés d'hôte** : une clé inconnue demande une confirmation armée ; une clé **modifiée** est toujours un
  refus bloquant, sans bouton « accepter », même avec `StrictHostKeyChecking no`.
- **Secrets** (mots de passe, passphrases) : jamais en clair sur disque, dans les logs, les événements ou les
  messages d'erreur. Ils sont mémorisés seulement à votre demande, dans le trousseau système, ou à défaut
  dans un vault chiffré (Argon2id + XChaCha20-Poly1305, `vault.toml`).
- **Processus durci** au démarrage : pas de core dump ; sous Linux, pas d'attache `ptrace` par un autre
  processus du même utilisateur (processus Rust). Code Rust sans `unsafe`.
- **GUI** : CSP stricte, capabilities Tauri minimales, le webview ne désigne jamais un chemin local.
- Le détail des décisions est dans `docs/STACK.md` (*Decision log*) ; les revues de chaque phase sont dans
  `docs/PLAN.md`.

## Risques résiduels connus

1. **GUI** : un secret tapé dans un dialogue transite par des chaînes JavaScript et par l'IPC JSON, qu'on ne
   peut pas effacer de la mémoire. Sous Linux, le processus WebKit (`WebKitWebProcess`) redevient traçable
   par le même utilisateur si `kernel.yama.ptrace_scope` vaut 0.
2. **Bibliothèque SSH** : russh garde des copies de mots de passe dans des `String` ordinaires le temps de
   l'authentification.
3. **`StrictHostKeyChecking no` / `accept-new`** (y compris via un `Include` partagé) : une clé **inconnue**
   est acceptée sans question, comme avec OpenSSH (journalisé). Une clé modifiée reste refusée.
4. **macOS, TUI** : pas de protection anti-`ptrace` (aucune API sûre) ; l'app GUI signée bénéficie du
   *hardened runtime*.
5. **CSP** : `style-src 'unsafe-inline'` est conservé (styles des composants).
6. **Logs de débogage** : `PORTICO_LOG=trace` journalise des détails internes de russh dans
   `portico-tui.log` (dossier de données, `0600`). À n'activer que pour un diagnostic.
7. **Presse-papiers** : « Copier » (logs, clé publique) met hôtes, utilisateurs et adresses dans le
   presse-papiers système, que des gestionnaires de presse-papiers peuvent conserver. Les options sensibles
   de la config effective (`ProxyCommand`, `SetEnv`…) ne sont jamais copiées.
8. **RSA** : l'avis RUSTSEC-2023-0071 (Marvin) vise le *déchiffrement* RSA, que Portico n'utilise pas
   (génération et signature aveuglée seulement). Ed25519 reste le type par défaut.
9. **Non pris en charge** : `ProxyCommand` et `Match exec` (refus explicite, rien n'est exécuté).
10. **Releases** : paquets Linux non signés (vérifiez `SHA256SUMS`) ; app macOS signée et notarisée
    seulement si les secrets de signature sont configurés (voir `docs/RELEASE.md`).
11. **Tunnels `-L` / `-D`** : l'écoute loopback est joignable par tout processus local, de n'importe quel
    utilisateur de la machine, sans authentification (comme OpenSSH). Un proxy SOCKS donne accès à tout ce
    que le serveur atteint, y compris ses services `localhost`, sous votre identité SSH. Exposé hors
    loopback (confirmation explicite), c'est un relais ouvert pour le réseau.
12. **Destinations SOCKS** : les destinations en échec apparaissent dans le panneau debug (donc dans
    « Copier ») et, en mode diagnostic, dans `portico-tui.log` ; elles révèlent une partie de la navigation.
