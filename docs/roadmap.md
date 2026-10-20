# Estensioni desktop

L'host resta uno: l'avvio abilita le capacità implementate e l'agente sceglie l'operazione. La disponibilità effettiva è pubblicizzata da windows_info. Non si seleziona un "tipo di agente" sulla macchina Windows.

## Prossima fase: strumenti MCP per il desktop

1. Implementato in 0.3.0: `desktop_screenshot`, desktop virtuale visibile via GDI, risoluzione e coordinate fisiche.
2. Implementati in 0.5.0: `desktop_click`, `desktop_move`, `desktop_scroll`, `desktop_type`, `desktop_key`. Prossimo passo: drag e selezione finestra.
3. `desktop_windows` e accessibilità con Windows UI Automation.
4. Implementato in 0.3.0: screenshot come contenuto immagine MCP con metadati separati.
5. Limiti su dimensioni/numero azioni, corrispondenza fra screenshot e coordinate, selezione monitor, indicatore di sessione attiva e arresto locale.

Il processo deve vivere nella sessione desktop dell'utente. Servizi in Sessione 0 richiedono un helper nella sessione interattiva. Desktop bloccato, UAC secure desktop, contenuti protetti e policy UIPI vanno trattati esplicitamente. Non promettere controllo di qualunque schermata solo perché il processo è elevato.

## Computer Use di Codex

La documentazione OpenAI descrive Computer Use sull'host dove è installata/configurata l'app e nel flusso di remote connections. Non documenta un endpoint pubblico per inviare a un binario terzo il backend nativo del plugin. Una connessione remota ufficiale a un host con Codex installato è un percorso diverso dal bridge portabile.

Winremote-mcp usa proprie operazioni desktop tramite MCP: Codex potrà scegliere screenshot e input come strumenti. Un eventuale futuro relay del plugin nativo richiede un'interfaccia supportata e verificabile, non una dipendenza da API interne.

Fonti:
- [Computer Use](https://learn.chatgpt.com/docs/computer-use)
- [Remote connections](https://learn.chatgpt.com/docs/remote-connections)
- [Computer use API e strumenti propri](https://developers.openai.com/api/docs/guides/tools-computer-use)

## Evoluzione dell'esecuzione

- Output progressivo e identificativi dei job.
- Cancellazione e gestione esplicita dei processi persistenti.
- ConPTY per applicazioni interattive.
- Implementati in 0.4.0: upload/download a blocchi e copia/spostamento remoti.
- Trasferimento ricorsivo fra cartelle locali e remote, resume dopo disconnessione, risultati paginati e stato di connessione locale.
- Integrazione MCP tramite SDK ufficiale quando servono ulteriori capacità protocollo.

## Prossima versione: invito tramite QR

Progettazione iniziale disponibile in [Connessione tramite QR](qr-connection-design.md): formato, UI, strumento MCP, limiti e piano di verifica.

Funzionalità pianificata; non presente nell'eseguibile v0.5.1.

- Mostrare una finestra compatibile con Windows 7 e successivi, con QR grande, invito testuale IP:CODICE e pulsante per copiarlo.
- Codificare nel QR lo stesso invito breve della console, includendo la porta quando necessaria. Riutilizzare durata della sessione e autenticazione esistenti.
- Consentire di salvare il QR come PNG, con contrasto e margini adatti alla decodifica da un'immagine o screenshot.
- Aggiungere sul lato agente lo strumento MCP proposto `bridge_connect_qr(image_path)`: un decoder QR estrae e valida l'invito, poi usa il pairing esistente con challenge HMAC e TLS vincolato al certificato autenticato. Prevedere limiti per dimensioni dell'immagine e contenuto decodificato.
- Conservare l'invito testuale come alternativa per il collegamento manuale.

La prima implementazione prevista usa QR. Data Matrix resta un'alternativa futura. La decodifica deve avvenire tramite uno strumento dedicato, senza dipendere dalla lettura visiva dei caratteri o dalla capacità del modello di interpretare direttamente il QR.
