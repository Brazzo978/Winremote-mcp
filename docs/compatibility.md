# Compatibilità Windows

## Target e capacità

Il requisito di progetto è Windows 7 x64 e successivi, includendo Server 2008 R2 e Server 2012/R2. La release 0.5.0 usava una build Rust standard per Windows moderno: non usarla come distribuzione legacy.

Dalla 0.5.1 la build portabile usa `x86_64-win7-windows-msvc` con la libreria standard Rust ricompilata e il CRT statico. La [documentazione Rust](https://doc.rust-lang.org/rustc/platform-support/win7-windows-msvc.html) descrive questo target Tier 3 e la necessità di build-std; non distribuisce una std precompilata. I [target Rust Windows standard](https://doc.rust-lang.org/rustc/platform-support.html) richiedono invece Windows 10 / Server 2016 o successivi.

Il caricamento dell'EXE non dipende direttamente da `SetThreadDpiAwarenessContext`. La funzione viene cercata in USER32 già caricata solo quando disponibile; nessun DLL esterno viene cercato sul disco per sostituirla.

- Windows 7, 8/8.1 e Server 2008 R2/2012/R2: comandi PowerShell, informazioni macchina e strumenti per file; desktop disabilitato.
- Windows 10 da 1607 e Server 2016 o successivi: anche screenshot e input, con sessione interattiva sbloccata.
- Windows 10 1607: contesto DPI per monitor V1; dalle versioni che supportano V2 viene preferito V2. Il contesto originale del thread viene ripristinato dopo ogni operazione.

`windows_info` pubblicizza solo le capacità disponibili sul sistema e aggiunge `desktop_supported` e `desktop_unavailable_reason`. Il catalogo MCP rimane stabile: invocare uno strumento desktop sul sistema legacy produce un errore operativo esplicito.

Il firewall usa `HNetCfg.FwPolicy2`, disponibile anche prima di Windows 7 secondo la [documentazione Microsoft](https://learn.microsoft.com/en-us/windows/win32/api/netfw/nn-netfw-inetfwpolicy2). La regola mantiene profilo Private, LocalSubnet, programma, IP locale e porta selezionati. Non dipende dai cmdlet NetSecurity assenti su Windows 7.

## Compilazione del binario portabile

Occorrono un host Windows moderno con MSVC Build Tools e SDK, Python 3 e la toolchain Rust indicata dallo script:

```powershell
rustup toolchain install nightly-2026-09-15 --profile minimal --component rust-src
.\scripts\Build-Win7.ps1
```

Lo script ricompila std per il target Windows 7, controlla gli import PE e produce `dist/winremote-mcp-win7-x64.exe`. La CI usa lo stesso percorso, esegue test unitari e smoke test con il binario legacy su un runner Windows moderno e conserva l'artifact.

`scripts/Build.ps1` e la build Rust standard rimangono utili per lo sviluppo su Windows moderno, ma non producono un binario dichiarabile Windows 7.

## Stato della verifica

I test locali della nuova gestione DPI coprono API assente, rifiuto dei contesti, fallback V1 e ripristino del contesto. I test delle capacità verificano che gli host legacy non pubblicizzino strumenti desktop. Il test firewall, se eseguito con privilegi elevati, crea una regola per il solo eseguibile di test su loopback, ne controlla i vincoli e verifica la rimozione senza aprire alcun listener.

Il controllo PE verifica intestazioni e import statici/delay-load e rifiuta le dipendenze moderne individuate. Non prova ogni percorso runtime o la presenza di ogni DLL transitiva. I risultati della compilazione e dei test sono nella [CI](https://github.com/Brazzo978/Winremote-mcp/actions).

Una prima prova reale su Server 2012 R2 ha verificato avvio, pairing, PowerShell 4 elevata, desktop disabilitato e upload/download con SHA-256 identico. La correzione di output UTF-8 e directory di lavoro è stata motivata da questa prova; richiede una nuova esecuzione con la build aggiornata. Windows 7 resta da verificare sul sistema effettivo. Inizia con `--version`, poi avvia il bridge e verifica `windows_info`, PowerShell, pairing e trasferimenti. Su tali sistemi la risposta attesa è `desktop_supported: false`. La versione PowerShell di sistema e le policy dell'host possono influire sugli script e sui programmi eseguiti.

Il cwd di PowerShell deve essere un percorso drive/UNC normale entro il limite legacy MAX_PATH: le directory di lavoro troppo lunghe o i percorsi volume senza drive vengono rifiutati esplicitamente. Questo limite riguarda cwd, non il limite dei trasferimenti. La console del bridge segnala pairing/richieste autenticate e ultimo contatto; vedi il README.
