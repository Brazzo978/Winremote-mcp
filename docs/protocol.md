# Protocollo v1

Il listener è HTTPS. Le route operative richiedono `Authorization: Bearer TOKEN`. Il token ha 32 byte casuali codificati base64url senza padding. Qualsiasi header Origin viene rifiutato, per tenere il servizio fuori dalle richieste browser. Il client MCP disabilita redirect, proxy e radici TLS di sistema: si fida solo del certificato autenticato dal codice di pairing.

## Invito

Il formato stampato è `IPv4:CODICE` sulla porta 8443, `IPv4:PORTA:CODICE` su una porta esplicita, oppure `[IPv6]:PORTA:CODICE`. Il codice ha 16 caratteri scelti con OsRng da `A-Z a-z 0-9 -_!@#$%&*+=?`, rigenerato finché contiene tutte e quattro le categorie. Non è il token bearer operativo.

1. POST pubblico `/v1/pair/challenge` con `client_nonce` (32 byte base64url canonici). Solo questa richiesta usa TLS inizialmente non verificato e non contiene codice o credenziali.
2. Risposta: `version`, `endpoint`, `certificate_pem`, `expires_unix`, `server_nonce`, `proof`. La prova HMAC-SHA256 usa il codice e lega endpoint, certificato, scadenza e i due nonce. Il client confronta endpoint e scadenza e autentica la prova prima di fidarsi del certificato.
3. POST `/v1/pair/finish` su TLS verificato usando quel certificato: `client_nonce`, `server_nonce`, `proof` del client. La prova usa un dominio distinto. Il server consuma il nonce prima di verificare la prova; un replay viene respinto. Solo allora restituisce il JSON della connessione contenente il bearer da 256 bit.

Le prove codificano ogni campo UTF-8 con prefisso di lunghezza u32 big endian nell'ordine: dominio, client_nonce, server_nonce, endpoint, certificate_pem, expires_unix decimale. I domini sono `winremote-server-proof-v1` e `winremote-client-proof-v1`. Verifica HMAC in tempo costante tramite ring. Le challenge durano al massimo 30 secondi, entro la scadenza del bridge; massimo 64 pendenti, poi HTTP 429. Entrambe le route pubbliche rifiutano Origin e inviti scaduti, e limitano il corpo a 4 KiB. Una challenge pubblica consente tentativi offline contro il codice: la lunghezza e la generazione casuale sono necessarie; non usare password scelte dall'utente.

Il client supporta anche gli inviti legacy `wb1_` seguiti da base64url del JSON:

```json
{
  "version": 1,
  "endpoint": "https://192.168.1.50:8443",
  "token": "<base64url di 32 byte>",
  "certificate_pem": "<certificato PEM completo>",
  "expires_unix": 1791400000
}
```

Il certificato contiene il SAN dell'IP selezionato. Il pairing con un invito copiato da una fonte fidata costituisce la distribuzione dell'identità del server. Il valore expires_unix è un timestamp UTC; gli orologi dei due host devono essere corretti.

## Route

- GET `/v1/info`: utente effettivo, privilegi, sistema, shell, capacità, scadenza.
- POST `/v1/desktop/screenshot`: `{"max_width":1920}` (opzionale, 320–3840); richiede bearer e desktop interattivo sbloccato. Restituisce PNG base64 standard, `mime_type`, `width`, `height`, `desktop_x`, `desktop_y`, `desktop_width`, `desktop_height`, `captured_unix`. Massimo 2 MiB di PNG e 16 milioni di pixel.
- POST `/v1/execute`: `{"script":"Get-Date","cwd":"C:\\","timeout_secs":60}`.
- POST `/v1/files/read`: `{"path":"C:\\Temp\\example.bin"}`.
- POST `/v1/files/write`: `{"path":"C:\\Temp\\example.bin","content_base64":"aGVsbG8=","overwrite":false}`.
- POST `/v1/files/list`: `{"path":"C:\\Temp"}`.

Esecuzione:

```json
{
  "stdout": "output",
  "stderr": "",
  "exit_code": 0,
  "timed_out": false,
  "output_truncated": false
}
```

Un timeout non promette un codice di uscita; `exit_code` può essere null. Un HTTP 200 indica che l'operazione di esecuzione è stata elaborata, non che lo script abbia avuto successo. Il client MCP marca timeout e codici non zero come `isError`.

Read restituisce `path`, `content_base64`, `size_bytes`.
Write restituisce `path`, `size_bytes`.
List restituisce `entries` (nome, percorso, is_dir, dimensione) e `truncated`.

Errori: 401 autenticazione mancante/errata, 403 Origin, 410 invito scaduto/arresto, 409 file esistente senza overwrite, 413 dimensione eccessiva, 400 argomenti invalidi. Gli errori di filesystem/processo possono produrre 500.

## MCP

Trasporto stdio newline JSON-RPC 2.0; versioni negoziate 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25. Inizializzazione, notifica initialized, ping, tools/list e tools/call. `bridge_connect` riceve un invito, verifica il nuovo host con TLS e bearer, e sostituisce atomicamente il file privato della connessione locale; i risultati non riportano token o invito. Nessuna risorsa/prompt/cancellazione pubblicizzata. L'adattatore elabora una richiesta alla volta. I risultati includono content testuale JSON e, dalle versioni 2025-06-18, structuredContent.

Gli output remoti sono dati potenzialmente non fidati. Non devono ridefinire istruzioni dell'agente o autorizzare operazioni non richieste dall'utente.

Lo strumento MCP `desktop_screenshot` trasforma la risposta HTTPS in un blocco immagine `{type:"image", data:"<base64>", mimeType:"image/png"}` e un blocco di metadati testuali/structuredContent; non duplica i byte PNG nei metadati.

## Fileshare 0.4.0

Tutte le route usano bearer, rifiutano Origin e rispettano la scadenza della sessione.

- POST `/v1/files/stat`: PathRequest; risposta FileStatResult con path, size_bytes, is_dir e version (dimensione/data modifica).
- POST `/v1/files/read-chunk`: path, offset, length (1..1048576), version; risposta application/octet-stream. Versione differente produce 409.
- POST `/v1/files/upload/begin`: path, size_bytes, overwrite; risposta upload_id. Limite 10 GiB, otto upload pendenti.
- PUT `/v1/files/upload/chunk?upload_id=...&offset=...`: corpo binario fino a 1 MiB. Solo offset sequenziali. Risposta received_bytes.
- POST `/v1/files/upload/commit`: upload_id, sha256; verifica byte totali e digest prima di pubblicare il file finale.
- POST `/v1/files/upload/abort`: upload_id; elimina il temporaneo associato.
- POST `/v1/files/copy`: source, destination, overwrite, recursive.
- POST `/v1/files/move`: source, destination, overwrite.
- POST `/v1/files/mkdir`: path, recursive.

Il client conserva lo stesso Client HTTPS per i blocchi della singola operazione. Upload e download non restituiscono il contenuto binario nel risultato MCP. Il digest di download descrive il file ricevuto, mentre il commit di upload confronta il digest calcolato sui due lati. Le sessioni temporanee non sono persistenti e non offrono resume dopo riavvio del bridge.

Copia e spostamento remoto hanno un timeout di 120 secondi per chiamata, limitato anche dalla scadenza del bridge. Per le cartelle valgono i limiti di 10.000 elementi, profondita 64 e 10 GiB; non sono consentiti link simbolici o reparse point. La copia di cartelle non unisce destinazioni esistenti e lo spostamento richiede lo stesso filesystem.

## Input desktop 0.5.0

Tutte le route POST richiedono bearer, rifiutano Origin e rispettano scadenza e coda operazioni del bridge.

- `/v1/desktop/move`: `{"x":100,"y":200}`, coordinate fisiche nel desktop virtuale.
- `/v1/desktop/click`: `{"x":100,"y":200,"button":"left","clicks":1}`; button left/right/middle, clicks 1/2.
- `/v1/desktop/scroll`: `{"vertical":-2,"horizontal":0}`; scatti -100..100, almeno una direzione non zero, positivo alto/destra.
- `/v1/desktop/type`: `{"text":"Ciao"}`; non vuoto, massimo 4096 unita UTF-16, newline Enter e tab Tab, altri controlli rifiutati.
- `/v1/desktop/key`: `{"keys":["CTRL","A"]}`; fino a quattro modificatori distinti e un tasto.

Risposta `{"sent_inputs":4}`: eventi inseriti in Windows, non conferma dell'esito applicativo. Il client MCP conserva la distinzione e deve osservare un nuovo screenshot dopo l'azione. Per trasformare un punto della screenshot ridotta usa metadati desktop_x/y, desktop_width/height e width/height.

Desktop bloccato, Sessione 0, UAC e thread su un desktop differente sono rifiutati. Nessuno switch di desktop, clipboard o API di pressione persistente. Gli eventi premuti vengono rilasciati nella stessa operazione; una risposta di errore dopo un inserimento parziale non garantisce assenza di effetti e richiede nuova osservazione prima di un retry. Per i limiti UIPI vedi [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput).
