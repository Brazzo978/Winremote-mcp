# Verifiche della versione 0.5.0

Distribuzione verificata: Windows x64. Le prove locali del 9 ottobre 2026 hanno usato la toolchain Rust gnullvm con LLVM-MinGW e il binario release. I risultati di CI MSVC sono consultabili nella [pagina Actions](https://github.com/Brazzo978/Winremote-mcp/actions).

## Verifiche locali completate

- `cargo fmt --all -- --check`: passato.
- `cargo clippy --offline --locked --all-targets -- -D warnings`: passato.
- `cargo test --offline --locked`: 35 test passati, nessun fallimento.
- `cargo build --release --offline --locked`: passato.
- `python tests/smoke.py --exe target/release/winremote-mcp.exe`: passato, incluso screenshot reale.
- `python tests/desktop_smoke.py --exe target/release/winremote-mcp.exe`: passato, input reali su una finestra temporanea di test.

Il test end-to-end usa un host HTTPS e il client MCP stdio reali su loopback. Copre pairing breve, certificato autenticato dal codice, nonce monouso e replay, invito errato, ACL della connessione, elenco dei 18 strumenti, PowerShell UTF-8, errori ed exit code, limiti di output, timeout e terminazione dei discendenti. Verifica inoltre la chiusura del listener alla scadenza.

I test dei trasferimenti includono un file da 8 MiB + 768 byte con confronto SHA-256 e byte su disco, file vuoti, rifiuto delle sovrascritture, conservazione della destinazione dopo un download fallito, creazione di cartelle, copia/spostamento remoto, copia ricorsiva e round trip CLI. I test unitari coprono offset errati, upload incompleti, digest errato e pulizia dei temporanei.

Per gli screenshot sono verificati il PNG reale, dimensioni e metadati, downscaling, ordine dei colori e rifiuto di richieste senza autenticazione, con Origin o parametri invalidi. La cattura sul remoto rimane in memoria.

## Input desktop

Il test locale su una finestra dedicata verifica coordinate fisiche, click sinistro/destro/centrale e doppio click, scroll verticale e orizzontale, testo Unicode con accenti, CJK ed emoji, newline e tab, Ctrl+A e Backspace. Controlla il rilascio dei modificatori, il rifiuto di input quando un modificatore o il tasto richiesto è già premuto, argomenti invalidi e un nuovo screenshot dopo l'azione. Il test ripristina puntatore e focus e rimuove finestra e temporanei.

Le route di input richiedono autenticazione e rifiutano Origin e sessioni scadute. I test unitari non iniettano input reali. L'invio del testo ricontrolla desktop e scadenza fra piccoli blocchi.

## Prove in LAN

Le prove reali fra due PC Windows hanno verificato pairing, PowerShell elevata, informazioni macchina, screenshot nativo, upload/download con confronto SHA-256, file vuoti, stat/list, mkdir, copia/spostamento e copia ricorsiva. Sono stati verificati anche autenticazione mancante, Origin, invito errato, conservazione della connessione valida dopo un pairing fallito, errori PowerShell e timeout.

Con la versione 0.5.0 sono stati verificati connessione e capacità del bridge e l'apertura di Chrome su YouTube tramite PowerShell. Non è ancora completata una prova degli input mouse/tastiera 0.5.0 sulla macchina in LAN; la loro verifica completa è locale.

Durante una prova RDP minimizzata, la cattura GDI ha restituito `StretchBlt` con errore Windows 5. Ripristinare la finestra RDP e tenere disponibile la sessione interattiva è il primo controllo suggerito. Un tentativo separato di cattura di una finestra via PowerShell ha funzionato, ma tale fallback non è implementato in `desktop_screenshot`.

Inviti, connessioni, inventari, nomi dei PC e screenshot privati delle prove non sono distribuiti.

## Binario iniziale della release

- Dimensione: 7.982.592 byte.
- SHA-256: `736eb1c103860513148c4bec3da1b3d2cfb53dac091ecc89985a97055757bcab`.
- Toolchain: `stable-x86_64-pc-windows-gnullvm`, LLVM-MinGW 20260922.
- Import: DLL di sistema Windows/UCRT, incluse `user32.dll` e `gdi32.dll`; nessuna DLL del compilatore aggiuntiva.

Questo digest identifica il binario locale già testato, distribuito nella prima release. Un'altra compilazione, inclusa quella MSVC della CI, può produrre un digest differente.

## Ambito delle verifiche

La CI esegue formattazione, Clippy, test unitari, smoke test e build release su Windows MSVC. Nei test smoke usa `--allow-headless`: se il runner non ha un desktop interattivo, le verifiche desktop vengono segnalate come saltate. Un risultato CI positivo non equivale quindi a una prova mouse/tastiera su tutte le sessioni Windows.

Screenshot e input richiedono il desktop interattivo sbloccato della sessione del bridge. Sessione 0, lock screen e desktop UAC protetto sono rifiutati. Focus, contenuti protetti e RDP possono influire sul risultato.

I limiti su durata, dimensioni, filesystem e processi sono riportati nel [README](../README.md#limiti-operativi). Non sono ancora implementati UI Automation, drag, trasferimenti riprendibili, processi persistenti o un relay del Computer Use nativo di Codex.
