# Guida rapida

Winremote-mcp collega due PC Windows: il **PC controllato**, su cui lavori a distanza, e il **PC dell'agente**, su cui gira Codex. Copia **lo stesso EXE** su entrambi, per esempio in C:\Tools\Winremote-mcp\winremote-mcp.exe. L'EXE portabile non richiede Rust sulle macchine che lo eseguono. Le due macchine devono potersi raggiungere in rete; sul PC controllato serve una sessione Windows interattiva per screenshot e input.

## 1. Avvia il PC controllato

Fai doppio clic su winremote-mcp.exe e approva la richiesta UAC locale. Lascia aperta la console: mostra un invito nel formato IP:CODICE, per esempio 192.168.1.50:Ab7!kP2@qR9#xT4?. Il codice contiene 16 caratteri; l'esempio è fittizio. Non pubblicare l'invito: consente di usare gli strumenti del bridge con i privilegi del processo sul PC controllato.

L'avvio con doppio clic sceglie l'IPv4 della route predefinita, ascolta sulla porta TCP **8443** per **un'ora** e crea una regola firewall limitata al profilo **Private** e a **LocalSubnet**. Una VPN o più schede di rete possono far scegliere l'IP sbagliato: in quel caso avvia host --bind come spiegato nel [README](../README.md). La regola non apre automaticamente le reti Public. Chiudendo la console o premendo Ctrl+C termini il bridge; ogni nuovo avvio richiede un nuovo invito.

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

Sostituisci YOUR_USER e, se necessario, il percorso dell'EXE con **percorsi assoluti reali del PC dell'agente**. Le stringhe TOML qui usano apici singoli per conservare i backslash di Windows. TOML non espande $env:LOCALAPPDATA, %LOCALAPPDATA% o ~ dentro command e args. Se esiste già una tabella [mcp_servers.winremote], modifica quella invece di duplicarla. Riavvia o riconnetti il server MCP in Codex dopo la modifica.

In alternativa, se usi **Codex CLI**, registralo da PowerShell sul PC dell'agente:

~~~powershell
codex mcp add winremote -- 'C:\Tools\Winremote-mcp\winremote-mcp.exe' mcp --connection "$env:LOCALAPPDATA\Winremote-mcp\connection.json"
codex mcp list
~~~

Il comando CLI espande la variabile d'ambiente in PowerShell prima di salvare il percorso. Usa **una** delle due modalità di registrazione per evitare una voce duplicata. La [documentazione ufficiale OpenAI su MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli) descrive i server stdio, config.toml e codex mcp add.

## 3. Connetti l'agente e verifica

Fornisci all'agente l'invito mostrato sul PC controllato e chiedigli di chiamare bridge_connect con l'argomento invitation. L'adattatore MCP salva sul PC dell'agente una connessione temporanea nel file configurato. Chiedi quindi windows_info e desktop_screenshot: il primo verifica la connessione, il secondo mostra il desktop visibile della sessione del bridge. Un prompt iniziale è:

> Usa bridge_connect con l'invito che ti fornisco. Poi esegui windows_info e desktop_screenshot e dimmi cosa riesci a vedere prima di agire.

L'adattatore espone **18 strumenti** per informazioni Windows, screenshot e input desktop, PowerShell, file e cartelle. Per file grandi usa file_upload e file_download (fino a **10 GiB** per file): local_path si riferisce al PC dell'agente, remote_path al PC controllato. Usa percorsi assoluti, crea prima le cartelle di destinazione e imposta overwrite: true soltanto se vuoi sostituire un file esistente. Per i dettagli degli strumenti vedi il [README](../README.md).

Anche un agente basato su DeepSeek o un altro modello può usare il bridge se il suo programma host supporta MCP stdio e avvia l'adattatore; il solo modello non stabilisce la connessione. Il progetto espone propri strumenti MCP per screenshot e input: non collega il motore nativo di Computer Use di Codex al PC remoto. Se qualcosa non funziona, consulta la [guida ai problemi comuni](troubleshooting.md).
