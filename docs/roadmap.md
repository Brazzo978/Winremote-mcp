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
