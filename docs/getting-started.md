# Guida rapida

Winremote-mcp collega due PC Windows: il **PC controllato**, su cui lavori a distanza, e il **PC dell'agente**, su cui gira Codex. Copia **lo stesso EXE** su entrambi, per esempio in C:\Tools\Winremote-mcp\winremote-mcp.exe. La release scaricabile è ancora 0.5.1; QR e approvazione locale sono nel sorgente 0.6.0 e richiedono una build aggiornata su entrambi i PC. La compatibilità dei nuovi percorsi con Windows 7 e Server 2012 R2 effettivi resta da verificare. Le due macchine devono potersi raggiungere in rete; sul PC controllato serve una sessione interattiva per la finestra di consenso e per screenshot/input.

## 1. Avvia il PC controllato

Con la 0.6.0, fai doppio clic su winremote-mcp.exe e approva UAC: appare una finestra nativa con QR, invito selezionabile, scadenza e pulsanti **Copia invito**, **Salva PNG...**, **Arresta bridge**. Lascia aperta anche la console: mostra lo stesso invito nel formato IP:CODICE, per esempio 192.168.1.50:Ab7!kP2@qR9#xT4?. Il codice contiene 16 caratteri; l'esempio è fittizio. Non pubblicare invito o PNG: consentono di usare il bridge con i privilegi del processo. Dal terminale, `host --qr` apre la finestra; `host` senza `--qr` resta nella console.

L'avvio con doppio clic sceglie l'IPv4 della route predefinita, ascolta sulla porta TCP **8443** per **un'ora** e crea una regola firewall limitata al profilo **Private** e a **LocalSubnet**. Una VPN o più schede di rete possono far scegliere l'IP sbagliato: in quel caso avvia `host --bind IP_LAN --qr` come spiegato nel [README](../README.md). La regola non apre automaticamente le reti Public. Chiudendo la console, premendo Ctrl+C o chiudendo la finestra termini il bridge; ogni nuovo avvio richiede un nuovo invito.

## 2. Configura Codex sul PC dell'agente

Installa l'EXE anche sul PC dell'agente e aggiungi questo blocco al file globale ~/.codex/config.toml di **quel PC**, conservando tutte le impostazioni già presenti:

~~~toml
[mcp_servers.winremote]
command = 'C:\Tools\Winremote-mcp\winremote-mcp.exe'
args = ["mcp", "--connection", 'C:\Users\YOUR_USER\AppData\Local\Winremote-mcp\connection.json']
startup_timeout_sec = 10
tool_timeout_sec = 3600
enabled = true
~~~

Sostituisci YOUR_USER e, se necessario, il percorso dell'EXE con **percorsi assoluti reali del PC dell'agente**. Le stringhe TOML qui usano apici singoli per conservare i backslash di Windows. TOML non espande $env:LOCALAPPDATA, %LOCALAPPDATA% o ~ dentro command e args. Se esiste già una tabella [mcp_servers.winremote], modifica quella invece di duplicarla. Riavvia o riconnetti il server MCP in Codex dopo la modifica. Prima di sostituire l'EXE sul PC dell'agente termina il processo MCP eventualmente in esecuzione: Windows può impedirne la sovrascrittura. Riavvialo dopo l'aggiornamento alla 0.6.0 per caricare i 21 strumenti.

In alternativa, se usi **Codex CLI**, registralo da PowerShell sul PC dell'agente:

~~~powershell
codex mcp add winremote -- 'C:\Tools\Winremote-mcp\winremote-mcp.exe' mcp --connection "$env:LOCALAPPDATA\Winremote-mcp\connection.json"
codex mcp list
~~~

Il comando CLI espande la variabile d'ambiente in PowerShell prima di salvare il percorso. Dopo la registrazione CLI, imposta anche tool_timeout_sec = 3600 nella tabella winremote del config.toml, come nell'esempio sopra, per consentire operazioni e trasferimenti più lunghi del timeout predefinito. Usa **una** delle due modalità di registrazione per evitare una voce duplicata. La [documentazione ufficiale OpenAI su MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli) descrive i server stdio, config.toml e codex mcp add.

## 3. Connetti l'agente e verifica

Fornisci all'agente l'invito mostrato sul PC controllato e chiedigli `bridge_connect` con `invitation`. In alternativa, con la 0.6.0 salva uno screenshot o il PNG sul **PC dell'agente** e usa `bridge_connect_qr` con `image_path` assoluto: un allegato in chat non è automaticamente leggibile dal processo MCP. Il QR può essere in un PNG o JPEG locale. L'adattatore salva sul PC dell'agente una connessione temporanea nel file configurato. Chiedi quindi `windows_info` per verificare connessione e capacità. Se `desktop_supported` è true, `desktop_screenshot` mostra il desktop visibile della sessione del bridge; su Windows precedenti a 10/Server 2016 screenshot e input sono disabilitati.

> Usa bridge_connect con l'invito che ti fornisco, oppure bridge_connect_qr con il percorso assoluto del PNG/JPEG locale. Poi esegui windows_info e acquisisci desktop_screenshot soltanto se desktop_supported è true.

Se vuoi comunicare soltanto l'IP, nella finestra Windows attiva **Ricevi richieste di accesso**. Chiedi all'agente `bridge_request_access` con `ip` (porta 8443 per default, `client_label` facoltativa). L'agente mostra il `verification_code` restituito con stato `pending`; confrontalo con quello nella finestra sul PC controllato, controlla IP osservato e privilegi, spunta la casella di conferma e premi **Approva**. Il nome dichiarato non è verificato. L'agente usa quindi `bridge_access_status` con il `request_id` locale; attende almeno `poll_after_secs` fra i controlli e può usare `wait_secs` fra 0 e 25. Finché lo stato è `pending`, non è connesso. Non comunicare password Windows. Le richieste scadono entro 120 secondi; **Rifiuta** o disattivare la ricezione annulla quelle in attesa. I client già collegati restano attivi finché il bridge non si arresta o scade.

L'adattatore 0.6.0 espone **21 strumenti** per pairing, informazioni Windows, screenshot e input desktop, PowerShell, file e cartelle. Per file grandi usa `file_upload` e `file_download` (fino a **10 GiB** per file): `local_path` si riferisce al PC dell'agente, `remote_path` al PC controllato. Usa percorsi assoluti, crea prima le cartelle di destinazione e imposta `overwrite: true` soltanto se vuoi sostituire un file esistente. Per i dettagli degli strumenti vedi il [README](../README.md).

Anche un agente basato su DeepSeek o un altro modello può usare il bridge se il suo programma host supporta MCP stdio e avvia l'adattatore; il solo modello non stabilisce la connessione. Il progetto espone propri strumenti MCP per screenshot e input: non collega il motore nativo di Computer Use di Codex al PC remoto. Se qualcosa non funziona, consulta la [guida ai problemi comuni](troubleshooting.md).
