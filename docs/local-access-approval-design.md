# Proposta: richiesta di accesso tramite IP e approvazione locale

Stato: progetto tecnico, non implementato. Estensione della [connessione QR](qr-connection-design.md).
Base: v0.5.1. Le route HTTPS e gli strumenti MCP riportati qui sono proposte, non disponibili nella release attuale.

## Obiettivo

L'utente comunica soltanto l'IP del PC Windows. L'agente richiede accesso; una persona davanti al PC approva o rifiuta. Non occorre fornire all'agente l'invito IP:CODICE o una password Windows.

La finestra di consenso appartiene al bridge sul PC da controllare. Il client MCP sul computer dell'agente avvia la richiesta e ne controlla lo stato.

## Esperienza prevista

1. L'utente avvia il bridge. La finestra include QR e interruttore locale Ricevi richieste di accesso.
2. L'agente chiama `bridge_request_access({"ip":"192.0.2.10"})`; porta predefinita 8443.
3. Il risultato MCP indica richiesta pendente e un codice di confronto. L'agente lo mostra all'utente nella chat.
4. Sul PC Windows appare una richiesta con IP sorgente effettivo, nome dichiarato, codice di confronto, privilegi che saranno concessi e scadenza.
5. L'utente confronta il codice con quello mostrato dall'agente e sceglie Approva oppure Rifiuta. Non deve trascrivere il codice.
6. L'agente chiama `bridge_access_status`. Dopo approvazione, il client completa la verifica HTTPS e salva la connessione.
7. Il risultato riporta capacità e scadenza, come il pairing attuale.

Esempio di finestra, con dati fittizi:

```text
Richiesta di accesso a questo PC
Da: 192.0.2.20
Nome dichiarato: Codex
Codice da confrontare: 7C2A9-1F4B8

Accesso: comandi e file con i privilegi attuali del bridge;
screenshot e input se disponibili.
Termine massimo: fine della sessione del bridge.

[ ] Il codice corrisponde a quello mostrato dal mio agente
[Approva]  [Rifiuta]
```

Approva resta disabilitato fino alla conferma del confronto. Rifiuta e chiusura del popup non autorizzano nulla. La finestra non si sposta ripetutamente in primo piano per ogni tentativo di rete.

Il nome dichiarato è un'etichetta non verificata, non una prova di identità. L'IP viene dal socket reale, mai da X-Forwarded-For o da campi dichiarati dal richiedente. NAT e relay possono far apparire lo stesso IP per più soggetti; il codice associa l'approvazione alla singola richiesta.

## Modalità di avvio e consenso

- Nel doppio clic la ricezione delle richieste è disponibile quando la UI interattiva è pronta; il controllo locale può disabilitarla.
- L'avvio esplicito `host` conserva il comportamento attuale. `host --qr` abilita la finestra; nella finestra l'operatore può consentire o disattivare le richieste.
- Se la UI non è disponibile, fallisce o viene chiusa, disabilitare questa modalità e invalidare le richieste pendenti. Il fallback console continua a usare invito e QR secondo il progetto principale.
- Non introdurre un'opzione di approvazione automatica, un'approvazione via route HTTP o una scelta remota del livello di privilegi.
- Approvare una richiesta non approva quelle successive e non crea un'associazione permanente.

La finestra deve chiarire che l'accesso usa i privilegi del bridge: se elevato, permette comandi amministrativi. La durata non supera il tempo rimanente della sessione host. Nella prima versione l'arresto del bridge revoca tutti gli accessi; la revoca selettiva per agente richiederebbe token e stato per agente, assenti oggi.

## Associazione della richiesta al canale

Il solo IP non autentica il PC e un certificato autofirmato scaricato dalla stessa connessione non basta a provarne l'identità. Il progetto richiede quindi confronto locale e legame crittografico alla richiesta prima di consegnare credenziali.

Approccio candidato, da formalizzare e verificare prima dell'implementazione:

1. Il client genera una chiave Ed25519 effimera e un nonce casuale da 32 byte. La chiave privata resta soltanto nella memoria del client MCP.
2. La richiesta iniziale HTTPS contiene solo chiave pubblica, nonce ed etichetta facoltativa. Non contiene invito, password o bearer.
3. Per questa sola richiesta iniziale il client può accettare il certificato autofirmato non ancora autenticato, ma deve acquisire il DER del certificato effettivamente presentato durante TLS. Non deve fidarsi del solo PEM restituito nel JSON.
4. Il server crea ID casuale, nonce proprio e scadenza. Finestra e client calcolano separatamente il medesimo codice di confronto dal contesto completo: versione e dominio, endpoint, DER del certificato TLS, chiave pubblica client, entrambi i nonce, ID, IP osservato e scadenza.
5. Il client usa il proprio DER osservato sul collegamento; l'host usa il proprio DER reale. Un relay che termina TLS con un certificato diverso deve produrre codici diversi. Il confronto umano nel passo precedente costituisce la verifica fuori banda.
6. Il contesto va serializzato canonicamente con prefissi di lunghezza, usando SHA-256 e domini distinti. La rappresentazione proposta del confronto usa 10 cifre esadecimali, divise in due gruppi. Il formato finale e la resistenza ai tentativi offline devono essere revisionati insieme ai limiti di richieste.
7. Per il polling e il ritiro, il client deve usare TLS vincolato al DER osservato e prove di possesso della propria chiave privata su nonce server freschi, monouso e legati a richiesta, contesto, azione e scadenza. Un ID richiesta non è una credenziale.
8. Solo dopo consenso locale e prova di possesso il server consegna la connessione. Non consegnarla attraverso il client bootstrap con verifica TLS disabilitata.
9. Verificare `/v1/info` e sostituire il file privato di connessione soltanto alla fine, riutilizzando le garanzie esistenti.

La verifica del certificato diventa autorevole attraverso il confronto umano, non attraverso il semplice pinning del primo certificato incontrato. Non descrivere la modalità come autenticata se il confronto viene saltato.

Questo documento definisce requisiti e approccio candidato, non un protocollo crittografico già validato. Prima di sviluppare la consegna del bearer servono specifica esatta dei messaggi, vettori di test e verifica dei casi MITM, relay, replay e collisione del confronto. Non abbreviare il progetto restituendo un token a chi conosce soltanto ID o IP.

Capacità disponibili già verificate nel sorgente locale: reqwest 0.12.28 offre `ClientBuilder::tls_info` e `TlsInfo::peer_certificate`; ring è già dipendenza del progetto e fornisce Ed25519. Non occorre aggiornare reqwest alla serie 0.13 per questo progetto.

Riferimenti: [metadati TLS reqwest](https://docs.rs/reqwest/latest/reqwest/tls/struct.TlsInfo.html), [chiavi Ed25519 in ring](https://docs.rs/ring/latest/ring/signature/struct.Ed25519KeyPair.html). Il modello richiesta, consenso su un altro dispositivo e polling ricorda il [Device Authorization Grant](https://www.rfc-editor.org/rfc/rfc8628.html); qui non viene proposta un'implementazione OAuth né una verifica dell'identità dell'agente tramite account.

## Contratto MCP proposto

Due strumenti, con lavoro di rete breve per chiamata, compatibili con l'adattatore sequenziale attuale.

### bridge_request_access

Argomenti: `ip` obbligatorio; `port` opzionale, default 8443; `client_label` opzionale, massimo 64 caratteri stampabili, senza controlli o markup. Rifiutare DNS, URL, IP non concreti e porte invalide.

Risultato proposto:

```json
{
  "status": "pending",
  "request_id": "<handle locale opaco>",
  "endpoint": "https://192.0.2.10:8443",
  "verification_code": "7C2A9-1F4B8",
  "expires_unix": 0,
  "poll_after_secs": 2,
  "instructions": "Show this code to the user; ask them to compare it on the Windows PC and approve there."
}
```

Il valore expires_unix dell'esempio è un segnaposto. Il risultato non contiene chiavi private o credenziali operative. L'handle esposto identifica una voce locale del client, che conserva in memoria chiave, certificato osservato e contesto; non coincide necessariamente con l'ID di rete.

### bridge_access_status

Argomenti: `request_id`, `wait_secs` facoltativo fra 0 e 25. Il client mantiene in memoria il contesto della richiesta; il polling non richiede una connessione operativa preesistente.

Stati: pending, approved/connected, denied, expired, unavailable. Dopo la consegna e il salvataggio, una lettura ripetuta dello stesso handle restituisce il risultato locale già completato, senza ripetere il ritiro remoto. Il risultato finale usa lo schema informativo di `bridge_connect`.

Pending è un risultato normale, non un errore. Backoff e attesa breve rispettano il tempo suggerito dal server. Non eseguire polling infinito. Il riavvio del client perde le richieste pendenti; occorre crearne una nuova, mentre la vecchia scade.

Il catalogo completo previsto conterebbe 21 strumenti: 18 attuali, uno QR e due per il consenso locale. Non occorre cambiare le configurazioni dei diversi modelli; il client MCP aggiornato espone le nuove capacità.

## Host: stato, limiti e ciclo di vita

Manager proposto `AccessRequestManager`, separato dalle challenge `PendingPair` già esistenti. Stati terminali e transizioni protetti dallo stesso lock: pending -> approved -> consumed, oppure denied/expired. Scadenza e arresto prevalgono sull'approvazione.

- Richiesta valida per massimo 120 secondi, comunque entro la sessione host. Tutte le durate sono controllate con deadline monotona.
- Massimo quattro richieste pendenti in totale, una per IP sorgente; massimo dieci nuove richieste al minuto complessive e tre per IP. Il numero di voci di rate limit deve essere limitato e ripulito, anche con molte sorgenti diverse.
- Polling non più frequente di due secondi; limite anche sulle challenge per le firme. Corpo massimo 4 KiB, risposte limitate.
- Tutte le route nuove rifiutano Origin e redirect/proxy nel client, come il pairing esistente.
- La UI mostra una coda limitata: nessuna creazione illimitata di finestre o code di messaggi.
- Prima di approvazione e prova valida: nessun bearer, esecuzione, file, screenshot o input.
- Disabilitazione locale, rifiuto, chiusura, timeout e arresto invalidano immediatamente le richieste interessate.
- L'UI comunica direttamente con il manager nello stesso processo; nessuna route remota permette di approvare.
- Non registrare chiavi, prove, token o contesti completi. La console può mostrare richiesta ricevuta, approvata o rifiutata con IP e ID non segreto.
- Se l'agente abbandona, nessuna approvazione o associazione resta valida oltre la deadline.

Route candidate: creazione richiesta, challenge per prova e polling/ritiro, tutte sotto `/v1/access/`. Le route operative restano invariate. Nomi e schema wire sono da fissare con la specifica crittografica, non introdotti nella documentazione del protocollo attuale come se fossero disponibili.

## Lavoro previsto e criteri di accettazione

1. Formalizzare messaggi, prova di possesso, legame TLS e confronto; aggiungere vettori positivi e negativi.
2. Implementare il manager con orologio controllabile e test delle corse: approvazione contro scadenza, doppio ritiro e arresto.
3. Collegare manager e finestra Win32, con confronto richiesto e rifiuto come scelta predefinita.
4. Aggiungere strumenti MCP, cache locale in memoria e finalizzazione condivisa con il pairing esistente.
5. Verificare bootstrap contro certificato sostituito, PEM nel JSON diverso dal certificato TLS, client key sostituita, nonce e contesti alterati, replay di firma, ID rubato e richiesta concorrente.
6. Verificare rifiuto/timeout senza modifica delle vecchie credenziali; approvazione senza prova corretta senza accesso; due agenti con richieste distinte; coda e rate limit sotto carico.
7. Provare nessun desktop/UI guasta, finestra chiusa, Server 2012 R2, Windows moderno e audit degli import Win7.
8. Documentare che QR e invito testuale continuano a funzionare e che questa modalità richiede aggiornamento sia host sia client.

Fuori scope: accesso senza presenza locale, memorizzazione permanente del consenso, identità account Codex/DeepSeek, approvazione da browser/telefono e revoca selettiva per agente.
