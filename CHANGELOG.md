# Changelog

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
