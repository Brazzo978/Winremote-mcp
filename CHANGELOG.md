# Changelog

## 0.5.1 — compatibilità Windows legacy

- Build dedicata Windows 7 x64 con std Rust ricompilata e runtime C statico.
- Rimosso l'import diretto di SetThreadDpiAwarenessContext che impediva l'avvio su Server 2012 R2.
- Desktop disabilitato quando l'API non è disponibile; comandi e file restano utilizzabili.
- Capacità desktop e motivazione esplicite in windows_info.
- Fallback DPI V1 per Windows 10 1607 e ripristino del contesto del thread.
- Firewall COM compatibile con Windows 7, con gli stessi limiti LAN e rimozione all'arresto.
- CI separata per la build legacy e controllo degli import PE.

La prova sul sistema Windows 7/Server 2012 R2 effettivo è ancora da completare; vedi [compatibilità](docs/compatibility.md).

## 0.5.0 — prima distribuzione pubblica

Bridge Windows x64 portabile e client MCP stdio nello stesso EXE, con 18 strumenti.

- Avvio locale con consenso UAC, invito breve `IP:CODICE`, scadenza e accesso firewall limitato alla LAN su reti Private.
- HTTPS con certificato per sessione, pairing HMAC e token operativo temporaneo.
- PowerShell con output strutturato, timeout e terminazione dei processi discendenti.
- Informazioni macchina e operazioni sul filesystem.
- Upload/download a blocchi fino a 10 GiB, copia/spostamento remoto e creazione di cartelle.
- Screenshot PNG come immagine MCP, coordinate del desktop virtuale e downscaling.
- Movimento, click, scroll, testo Unicode e combinazioni di tasti tramite SendInput.
- Guide Codex, configurazione stdio per altri client, troubleshooting, protocollo e verifiche.
- CI Windows per formattazione, Clippy, test e compilazione.

Le versioni 0.2.0–0.4.0 erano iterazioni locali di sviluppo. Il dettaglio delle prove della 0.5.0 e dei limiti attuali è in [docs/validation.md](docs/validation.md).
