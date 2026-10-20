# Winremote-mcp

Bridge Rust portabile per esporre temporaneamente una macchina Windows a Codex o a un altro agente tramite MCP.

**Avvio sulla macchina da controllare:** nel sorgente 0.6.0, doppio clic su `winremote-mcp.exe`, consenso UAC e finestra nativa con QR e invito breve `IP:CODICE`; la console resta aperta. Il programma seleziona l'IPv4 della route predefinita, ascolta su TCP 8443 per un'ora e apre una regola firewall limitata a reti Private e LocalSubnet. Le capacità disponibili dipendono dalla versione di Windows; l'agente le legge con `windows_info`.

**Sulla macchina dell'agente:** lo stesso binario esegue un adattatore MCP locale su stdio. Si configura una volta in Codex; il pairing salva una connessione temporanea in un file privato. Non servono account Windows remoti né OpenSSH.

## Download e avvio rapido

La release pubblicata è ancora [v0.5.1](https://github.com/Brazzo978/Winremote-mcp/releases/tag/v0.5.1). Contiene le funzioni precedenti, ma non QR o approvazione locale. Il sorgente 0.6.0 aggiunge queste funzioni: per usarle serve aggiornare sia host sia client MCP e riavviare il client per caricare il nuovo catalogo. Il pacchetto della 0.5.1 contiene EXE, guide, licenza, configurazioni di esempio e checksum.

1. Copia l'EXE sul PC Windows da controllare, avvialo e conferma UAC. Nel sorgente 0.6.0 lascia aperte console e finestra QR.
2. Sul PC Windows dell'agente, copia lo stesso EXE in un percorso stabile e registra il client MCP.
3. Nella 0.6.0 passa l'invito per `bridge_connect`, un'immagine PNG/JPEG locale per `bridge_connect_qr`, oppure il solo IP per chiedere accesso con approvazione sul PC Windows.
4. L'agente verifica le capacità con `windows_info`. Chiudi il bridge con Ctrl+C, la X della finestra o **Arresta bridge** al termine.

**Guide:** [installazione e configurazione MCP](docs/getting-started.md) · [problemi comuni](docs/troubleshooting.md) · [protocollo](docs/protocol.md) · [verifiche e limiti](docs/validation.md) · [contribuire e compilare](CONTRIBUTING.md) · [changelog](CHANGELOG.md).

Il progetto è in sviluppo: i limiti operativi riportati sotto fanno parte del comportamento attuale.

## Compatibilità Windows

Il binario portabile distribuito nella 0.5.1 è compilato per `x86_64-win7-windows-msvc`, con libreria standard Rust ricompilata e runtime C statico. Windows 7 x64 resta il target del progetto; la nuova UI e i nuovi percorsi 0.6.0 richiedono ancora verifica su Windows 7 e Server 2012 R2 effettivi.

| Sistema | PowerShell e file | Screenshot e input |
|---|---|---|
| Windows 7 / Server 2008 R2 | Build dedicata, prova sul sistema da completare | Disabilitati |
| Windows 8/8.1 / Server 2012/2012 R2 | Build dedicata, prova sul sistema da completare | Disabilitati |
| Windows 10 da 1607 / Server 2016 e successivi | Verificati su Windows moderno | Disponibili con sessione interattiva sbloccata |

`windows_info` riporta `desktop_supported`, `desktop_unavailable_reason` e le sole capacità disponibili per il sistema. Il catalogo MCP del sorgente 0.6.0 ha 21 strumenti. Su un host legacy, le chiamate desktop restituiscono un errore esplicito e gli strumenti per comandi/file continuano a essere disponibili; la finestra QR e consenso è separata dalle capacità di screenshot/input remoto.

Le API desktop moderne sono risolte a runtime: la loro assenza non impedisce il caricamento dell'EXE. La CI controlla anche gli import della build Windows 7 per rilevare dipendenze statiche incompatibili. Questi controlli non sostituiscono una prova sul sistema operativo di destinazione. Vedi [compatibilità e build legacy](docs/compatibility.md).

## Stato del sorgente 0.6.0

Implementato:

- HTTPS con certificato nuovo per ogni avvio, fidato esclusivamente attraverso l'invito.
- Token casuale da 256 bit e scadenza, verifica di autenticazione su ogni endpoint operativo.
- PowerShell elevata se il bridge è elevato; output, codice di uscita e timeout.
- Lettura/scrittura di file binari e lista directory.
- Upload/download di file fino a 10 GiB in blocchi binari HTTPS da 1 MiB; file finalizzati dopo trasferimento completo.
- Copia di file/cartelle sul remoto, spostamento e creazione di cartelle.
- Screenshot PNG del desktop virtuale Windows, restituito come immagine MCP con dimensioni e coordinate originali.
- Input desktop: movimento, click sinistro/destro/centrale e doppio click, scroll verticale/orizzontale, testo Unicode e combinazioni di tasti.
- Avvio di PowerShell sospeso, assegnazione a un Windows Job Object, quindi esecuzione; terminazione dei discendenti a fine comando o timeout.
- Adattatore MCP con elenco strumenti e risultati strutturati.
- Finestra nativa all'avvio con doppio clic: QR, invito selezionabile, scadenza, **Copia invito**, **Salva PNG...**, **Arresta bridge** e consenso locale. L'avvio esplicito `host --qr` abilita la stessa finestra.
- `bridge_connect_qr`: decodifica locale di PNG/JPEG e pairing con l'invito breve contenuto nel QR.
- `bridge_request_access` e `bridge_access_status`: richiesta tramite IP, confronto del codice e approvazione nella finestra Windows, senza condividere una password.

Gli inviti legacy `wb1_...` restano accettati dal client; il nuovo host stampa soltanto il formato breve. Per usarlo occorre aggiornare sia host sia client MCP.

Previsto, non ancora implementato:

- Drag e Windows UI Automation per elenco finestre, accessibilita e selezione esplicita della finestra.
- Terminale interattivo ConPTY, output progressivo, job persistenti e cancellazione tramite MCP.
- SSH opzionale, servizio Windows e helper per la sessione desktop.
- Il relay del Computer Use nativo di Codex non è implementato. Il bridge espone propri strumenti desktop MCP.

Il server non è un ambiente isolato: PowerShell e operazioni sui file hanno i privilegi dell'utente effettivo del bridge. L'invito concede tali capacità fino alla scadenza o all'arresto del processo.

## Uso del binario

### 1. Macchina Windows da controllare

Esegui il binario senza argomenti. Conferma il consenso UAC locale. Nel sorgente 0.6.0 si apre la finestra QR e l'invito resta anche nella console. Esempio: `192.168.1.50:Ab7!kP2@qR9#xT4?`. Il codice casuale ha 16 caratteri e contiene almeno una maiuscola, una minuscola, una cifra e un simbolo tra `- _ ! @ # $ % & * + = ?`. Con una porta diversa da 8443 il formato è `IP:PORTA:CODICE`. I due punti separano i campi e non fanno parte del codice.

Per scegliere esplicitamente IP e porta, usa un terminale elevato:

```powershell

.\winremote-mcp.exe host --bind 192.168.1.50 --port 8443 --ttl-secs 3600 --qr

```

Per un test locale con i soli privilegi dell'utente corrente:

```powershell

.\winremote-mcp.exe host --bind 127.0.0.1 --port 18443 --require-admin false --ttl-secs 300

```

Il default del sottocomando `host` è loopback e non apre la finestra; `--qr` la abilita esplicitamente. L'avvio senza argomenti seleziona invece un IP raggiungibile secondo la route IPv4 predefinita: con VPN o più schede, usa `--bind` per scegliere la LAN corretta. La regola firewall automatica viene aggiunta solo con host elevato e IP diverso da loopback, e consente rete Private + LocalSubnet. Le policy aziendali possono impedirla. Le reti Public non vengono aperte automaticamente.

Arresta con Ctrl+C, la X della finestra o **Arresta bridge**. Dopo la scadenza il listener si chiude. Ogni nuovo avvio genera token e certificato diversi: occorre un nuovo pairing.

### 2. Macchina di Codex: connessione e pairing

Puoi configurare prima il client MCP e dare l'invito all'agente, chiedendogli di usare lo strumento `bridge_connect`. Il client verifica il server e salva la connessione privata; un nuovo invito sostituisce atomicamente la connessione precedente. Non serve un pairing manuale a ogni avvio.

Con la 0.6.0 puoi passare all'agente un PNG o JPEG contenente il QR e chiedere `bridge_connect_qr` con `image_path`, percorso **assoluto sul PC dell'agente**. Se l'immagine è allegata alla chat ma il processo MCP non può leggerla, salvala prima su quel PC. Il decoder locale accetta solo file regolari PNG/JPEG fino a 8 MiB, immagini fino a 8192 pixel per lato e 16.777.216 pixel totali; ignora QR estranei e rifiuta inviti validi diversi nella stessa immagine. Il pulsante **Salva PNG...** scrive l'invito nel percorso scelto sul PC Windows: tratta quel file come una credenziale e cancellalo quando non serve più. **Copia invito** mette il testo negli appunti solo quando lo premi.

Per chiedere accesso senza comunicare l'invito, mantieni aperta la finestra Windows e attiva **Ricevi richieste di accesso**. L'agente chiama `bridge_request_access` con l'IP del PC controllato; la porta predefinita è 8443 e `client_label` è facoltativa. Il risultato `pending` include `verification_code` e un `request_id` locale opaco. Mostra il codice all'utente: sul PC Windows confrontalo con quello nella richiesta, verifica l'IP sorgente osservato e i privilegi indicati, spunta **Il codice corrisponde a quello mostrato dal mio agente** e premi **Approva**. Il nome dichiarato dal client non è verificato. Poi l'agente chiama `bridge_access_status` con `request_id`, rispettando `poll_after_secs` (almeno due secondi); `wait_secs` opzionale va da 0 a 25. `pending` non significa connesso. **Rifiuta**, scadenza o disattivazione della ricezione chiudono la richiesta senza creare una connessione. La modalità richiede la finestra: se la UI non si avvia, resta disponibile l'invito testuale.

Le richieste scadono entro 120 secondi e comunque alla fine della sessione; al massimo quattro sono pendenti, con limite di dieci nuove richieste al minuto e tre per IP. Disattivare **Ricevi richieste di accesso** cancella solo le richieste in attesa: i client già connessi conservano il bearer condiviso della sessione. **Arresta bridge** o la scadenza revocano tutti gli accessi; la revoca selettiva per client non è disponibile. Non condividere password Windows o il codice dell'invito per questo flusso.

Per il pairing manuale opzionale, crea la connessione con lo stesso binario, poi incolla l'invito al prompt. Evita di mettere il token nella riga di comando, nel repository o nei messaggi di log.

```powershell

.\winremote-mcp.exe pair --connection "$env:LOCALAPPDATA\Winremote-mcp\connection.json"

```

Il codice autentica il certificato del server tramite HMAC-SHA256 e nonce nuovi; solo dopo la verifica il client apre una connessione TLS fidata e riceve il token operativo da 256 bit. Il codice e il token non vengono inviati nella richiesta iniziale. Il pairing verifica TLS e autenticazione prima di scrivere. Su Windows il file viene creato con ACL dell'utente corrente prima di inserirvi il token; su Unix viene creato con permessi 0600. Il comando manuale `pair` non sovrascrive un file esistente: scegli un nuovo percorso o elimina esplicitamente il vecchio file. Lo strumento MCP `bridge_connect` sostituisce invece atomicamente la connessione configurata, dopo avere verificato il nuovo host. La connessione è conservata sul disco come credenziale privata, non cifrata con DPAPI.

Puoi registrare una sola volta il client MCP in Codex:

```toml

[mcp_servers.winremote]
command = 'C:\Tools\Winremote-mcp\winremote-mcp.exe'
args = ["mcp", "--connection", 'C:\Users\YOUR_USER\AppData\Local\Winremote-mcp\connection.json']
startup_timeout_sec = 10
tool_timeout_sec = 3600

```

Copia il blocco in `~/.codex/config.toml`, sostituendo i percorsi assoluti, o usa le impostazioni MCP dell'app. Riavvia/riconnetti il server MCP dopo la configurazione. Il client può inizializzarsi anche senza host disponibile; gli strumenti restituiscono un errore finché il pairing non è valido.

La configurazione MCP stdio di Codex è descritta nella [documentazione ufficiale](https://developers.openai.com/codex/mcp).

### 3. L'agente sceglie gli strumenti

Un prompt iniziale utile:

> Usa gli strumenti winremote per leggere le informazioni della macchina Windows. Poi esegui il lavoro richiesto su quella macchina usando percorsi assoluti Windows.

| Strumento | Argomenti |
|---|---|
| `bridge_connect` | `invitation` (invito breve `IP:CODICE`) |
| `bridge_connect_qr` | `image_path` assoluto sul PC dell'agente, PNG/JPEG locale |
| `bridge_request_access` | `ip` concreto; `port` opzionale (8443), `client_label` opzionale |
| `bridge_access_status` | `request_id` locale; `wait_secs` opzionale (0–25) |
| `windows_info` | nessuno |
| `desktop_screenshot` | `max_width` opzionale, default 1920 (320–3840) |
| `desktop_move` | `x`, `y` fisici nel desktop virtuale |
| `desktop_click` | `x`, `y`, `button` opzionale left/right/middle, `clicks` opzionale 1/2 |
| `desktop_scroll` | `vertical`, `horizontal` opzionali: scatti da -100 a 100 |
| `desktop_type` | `text`, massimo 4096 unita UTF-16 |
| `desktop_key` | `keys`, modificatori seguiti da un tasto: esempio CTRL + A |
| `powershell_execute` | `script`, `cwd`, `timeout_secs` opzionale |
| `file_read` | `path` assoluto |
| `file_write` | `path`, `content_base64`, `overwrite` opzionale |
| `directory_list` | `path` assoluto |
| `file_stat` | `path` remoto assoluto |
| `file_upload` | `local_path`, `remote_path`, `overwrite` opzionale |
| `file_download` | `local_path`, `remote_path`, `overwrite` opzionale |
| `file_copy` | `source`, `destination`, `overwrite`, `recursive` opzionali |
| `file_move` | `source`, `destination`, `overwrite` opzionale |
| `directory_create` | `path`, `recursive` opzionale |

`file_read` e `file_write` usano base64 standard RFC 4648; i trasferimenti grandi usano blocchi binari HTTPS. Nonce, prove di pairing, token e inviti legacy usano base64url senza padding. `overwrite` è false per default.

## Trasferimenti per agenti

Il fileshare usa la stessa connessione HTTPS autenticata del bridge e lo stesso invito breve. L'agente trasferisce il file tramite `file_upload` o `file_download`; MCP restituisce percorsi, dimensione e SHA-256, senza inserire il contenuto del file nel contesto del modello. `local_path` indica la macchina su cui gira il client MCP; `remote_path` indica la macchina del bridge. Entrambi i percorsi devono essere assoluti e la cartella di destinazione deve esistere.

Esempio di richiesta all'agente:

> Copia C:\Installers\programma.msi da questa macchina a C:\Temp\programma.msi sulla macchina remota. Poi esegui l'installazione richiesta e verifica l'esito.

Il trasferimento invia blocchi binari da massimo 1 MiB, con limite di 10 GiB per file e memoria limitata. Gli upload usano un file temporaneo nella cartella di destinazione e vengono finalizzati dopo controllo di dimensione e SHA-256. I download usano un file temporaneo locale, controllano la versione remota durante la lettura e calcolano il digest del file ricevuto. Non sono snapshot di file modificati da altre applicazioni; per trasferire dati attivi, crea prima una copia stabile.

`overwrite` e `recursive` sono false per default. Con `file_copy` puoi copiare una cartella remota impostando `recursive: true`; le cartelle di destinazione esistenti non vengono unite o sostituite. Copia e spostamento di cartelle limitati a 10.000 elementi, profondita 64 e 10 GiB; link simbolici e reparse point vengono rifiutati. `file_move` sposta nella stessa unita/filesystem; per un'altra unita usa la copia. `directory_create` crea una cartella o, con `recursive: true`, anche i genitori mancanti.

Per un trasferimento manuale con lo stesso EXE:

```powershell
.\winremote-mcp.exe upload --connection "$env:LOCALAPPDATA\Winremote-mcp\connection.json" --local "C:\Installers\programma.msi" --remote "C:\Temp\programma.msi"
.\winremote-mcp.exe download --connection "$env:LOCALAPPDATA\Winremote-mcp\connection.json" --remote "C:\Temp\risultato.zip" --local "C:\Downloads\risultato.zip"
```

Il listener rimane sulla porta 8443: non servono porte dati aggiuntive. Questa interfaccia fileshare e progettata per MCP/HTTPS; SCP e SFTP richiederebbero un server SSH distinto. Per trasferimenti lunghi imposta `tool_timeout_sec = 3600` nel client MCP; la scadenza effettiva del bridge resta comunque valida.

## Screenshot remoto

Lo strumento `desktop_screenshot` cattura ciò che è visibile sul desktop virtuale della sessione del bridge, includendo più monitor. Usa GDI di Windows ed esporta PNG in memoria: sul PC remoto non salva un file. MCP restituisce un blocco immagine `image/png` e metadati separati, così il client può mostrare l'immagine all'agente.

`max_width` riduce la larghezza mantenendo il rapporto d'aspetto e non ingrandisce l'immagine. I metadati contengono dimensioni originali del desktop e origine, che può essere negativa. La conversione dalle coordinate dell'immagine a quelle del desktop è `desktop_x + image_x * desktop_width / width` e analogamente per y.

È necessaria una sessione interattiva sbloccata; il modulo rifiuta Sessione 0, lock screen e desktop protetto UAC. Finestre coperte non vengono mostrate per intero; contenuti protetti possono apparire neri. Massimo 16 milioni di pixel nell'immagine ridotta e 2 MiB nel PNG: il modulo riduce ulteriormente la risoluzione se il PNG supera il limite.

Per usare i nuovi strumenti 0.6.0 aggiorna host e client MCP, poi riavvia il processo MCP per ricaricare il catalogo. Un EXE client già in esecuzione può impedire di sovrascrivere il file: arresta quel processo prima dell'aggiornamento. Il pairing breve resta invariato.

## Input desktop

I cinque strumenti di input usano SendInput di Windows nella sessione del bridge. L'agente puo osservare lo screenshot, inviare un'azione e acquisire un nuovo screenshot per controllarne l'effetto. La risposta `sent_inputs` conferma quanti eventi Windows ha inserito; non prova che l'applicazione li abbia elaborati o che il lavoro sia riuscito.

`desktop_move` e `desktop_click` ricevono pixel fisici del desktop virtuale, non coordinate dell'immagine ridotta. Converti con `x = desktop_x + image_x * desktop_width / width` e analogamente per y, arrotondando a interi. Sono possibili origini negative con piu monitor. Il click sposta prima il puntatore; `button` e left per default, `clicks` e 1 per default e puo essere 2.

`desktop_scroll` agisce alla posizione corrente del puntatore: spostalo sulla zona da scorrere. Gli scatti positivi verticali vanno verso l'alto; quelli orizzontali verso destra. Almeno una direzione deve essere diversa da zero, massimo 100 scatti per direzione in ogni chiamata.

`desktop_type` invia testo Unicode alla superficie che ha il focus, senza usare o modificare gli appunti. Massimo 4096 unita UTF-16 per chiamata; newline invia Enter, tab invia Tab. Questi tasti possono cambiare focus o attivare comandi nell'applicazione: per testi lunghi o importazioni preferisci un file. NUL e altri caratteri di controllo non sono accettati.

`desktop_key` invia una combinazione completa, premendo i modificatori e rilasciando i tasti al termine. Esempi: `{"keys":["CTRL","A"]}`, `{"keys":["ALT","F4"]}`, `{"keys":["ENTER"]}`. I modificatori CTRL/ALT/SHIFT/WIN sono distinti e precedono esattamente un tasto; sono accettati nomi comuni come Enter, Escape, Tab, Backspace, Delete, frecce, Home, End, PageUp, PageDown, Space, Insert, F1-F24, A-Z e 0-9.

Gli input richiedono il desktop Default interattivo sbloccato. Non commutano desktop e non interagiscono con lock screen, Sessione 0 o desktop UAC protetto. La destinazione dipende da focus e posizione del puntatore; questa versione non seleziona una finestra tramite handle o accessibilita. Evita input contemporanei dell'utente; modificatori o pulsanti gia premuti possono far rifiutare l'operazione. Windows applica inoltre UIPI: input verso processi di integrita superiore possono essere bloccati. Vedi la [documentazione Microsoft di SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput).

Non sono implementati drag, pressione persistente di tasti/pulsanti o UI Automation. Il consenso per una nuova sessione resta l'avvio locale del bridge e il pairing temporaneo.

## Limiti operativi

- Windows PowerShell di sistema, avviata senza profilo e in modalità non interattiva. Su Windows 7 la versione preinstallata è precedente alla 5.1: verifica i requisiti degli script e dei programmi che vuoi eseguire.
- Ogni chiamata avvia un processo nuovo; directory e variabili non persistono fra chiamate.
- Script: massimo 8 KiB, per rispettare il limite della riga di comando Windows.
- Timeout: 1–120 secondi, default 60, comunque limitato alla vita residua dell'invito.
- Output: massimo 1 MiB per stdout e 1 MiB per stderr; il flusso eccedente viene drenato e marcato come troncato.
- `file_read`/`file_write`: massimo 2 MiB; `file_upload`/`file_download`: massimo 10 GiB. Richieste HTTP e righe MCP: massimo 4 MiB; i file grandi viaggiano in blocchi binari.
- Lista directory: massimo 1000 elementi con indicatore di troncamento.
- Copia e spostamento remoto: massimo 120 secondi per chiamata, comunque entro la scadenza del bridge.
- Una operazione mutante/file/comando alla volta; più client con lo stesso invito condividono la coda.
- I processi avviati da un comando vengono terminati alla fine del comando: non usare questa versione per avviare servizi o applicazioni che devono restare aperte.
- Gli errori PowerShell terminanti producono codice 1; il codice dell'ultimo processo nativo viene propagato. Uno script può gestire esplicitamente le eccezioni o usare `exit`. L'exit code non rappresenta tutti gli errori applicativi possibili.
- Nessuna cancellazione esplicita MCP nella prima versione; valgono timeout e arresto del bridge.
- Nessun limite a una cartella è promesso: `cwd` stabilisce dove parte il comando, non è una sandbox.
- Il file della connessione contiene endpoint, token e certificato; il client non utilizza proxy di sistema o redirect HTTP.
- La regola firewall viene rimossa durante l'arresto normale. Un'interruzione forzata o crash può lasciare una regola `Winremote-mcp-*`: è limitata a programma/IP/porta e non mantiene attivo il server. Lo script di pulizia rimuove solo regole del progetto.
- Trasporto IPv4 nella selezione automatica; il sottocomando host accetta anche un IPv6 concreto. Connettività fra due macchine e consenso UAC richiedono una verifica sul sistema di destinazione.

## Compilazione e verifiche

Serve Rust e un toolchain Windows con compilatore C/linker (MSVC Build Tools o LLVM-MinGW/MinGW appropriato). PowerShell è necessaria a runtime solo sulla macchina Windows host, e su Windows per impostare le ACL durante il pairing.

```powershell

cargo build --release --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings

```

La CI verifica build standard e target Windows 7. Per distribuire il binario portabile usa `scripts/Build-Win7.ps1`.

`python tests/smoke.py --exe target/debug/winremote-mcp.exe` esegue un test end-to-end locale senza elevazione né apertura della LAN: TLS, autenticazione, file grandi, copia/spostamento, PowerShell, timeout, isolamento dei processi, screenshot e protocollo MCP. I test non avviano il percorso UAC senza argomenti. `python tests/desktop_smoke.py --exe target/debug/winremote-mcp.exe` verifica input reali contro una finestra temporanea di test: coordinate, click, scroll, Unicode, combinazioni e rilascio dei tasti. Sui runner CI senza desktop interattivo usa `--allow-headless`, che segnala esplicitamente le verifiche desktop saltate.

## Architettura

```text

Codex / altro host MCP
    -> winremote-mcp mcp (stdio locale)
        -> HTTPS autenticato nella LAN
            -> winremote-mcp host (Windows)
                -> PowerShell e filesystem

```

MCP non lega il progetto a un modello: anche DeepSeek può usare il bridge tramite un runtime agente che supporti MCP o tramite la sua API HTTPS. Il solo modello/API di inferenza non apre una connessione alla macchina.

Il protocollo è documentato in [docs/protocol.md](docs/protocol.md), e le estensioni desktop in [docs/roadmap.md](docs/roadmap.md).

## Avvisi nella console del bridge

La console sulla macchina controllata mostra un avviso quando un agente completa il pairing o quando un nuovo IP effettua la prima richiesta autenticata. Usa l'IP del socket, senza fidarsi di header inviati dal client. Gli orari sono indicati in UTC.

Ogni minuto mostra l'ultimo contatto autenticato per gli IP registrati, fino a un massimo di 64. HTTPS usa richieste separate: l'ultimo contatto indica attività osservata, non una connessione persistente o una prova di disconnessione. Nei messaggi non vengono inseriti token, inviti o contenuti dei comandi. Gli avvisi sono su stderr; lo stdout del client MCP rimane riservato al protocollo.
