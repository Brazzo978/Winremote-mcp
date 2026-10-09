# Contribuire a Winremote-mcp

Il progetto usa Rust e supporta come distribuzione verificata Windows x64. Per usare una release non serve compilare.

## Build

Per compilare occorrono Rust, un compilatore C/linker Windows compatibile con la toolchain scelta e Python 3 per gli smoke test. La CI usa MSVC su `windows-latest`; una build locale è stata verificata anche con gnullvm e LLVM-MinGW. Le impostazioni in `.cargo/config.toml` collegano staticamente il runtime C.

```powershell
cargo build --release --locked
.\scripts\Build.ps1
```

Lo script copia il binario in `dist/winremote-mcp.exe`. `dist/` e `target/` sono esclusi da Git.

## Verifiche

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --locked
python tests/smoke.py --exe target/debug/winremote-mcp.exe
python tests/desktop_smoke.py --exe target/debug/winremote-mcp.exe
```

Il primo smoke avvia bridge e client su loopback senza elevazione né apertura della LAN. Il test desktop crea una finestra temporanea e invia input reali: eseguilo in una sessione sbloccata, senza interagire con mouse e tastiera durante la prova. Su runner senza desktop usa `--allow-headless`, che segnala le verifiche saltate.

## Modifiche

Descrivi il problema e il comportamento risultante nella pull request. Mantieni i limiti espliciti nel protocollo e aggiorna le guide quando cambiano argomenti degli strumenti, autenticazione, filesystem o desktop. Per modifiche che toccano privilegi, pairing o input, verifica anche i casi di rifiuto e la pulizia dopo errore.

Non allegare inviti, token, `connection.json`, chiavi o certificati di sessione, inventari personali e screenshot delle macchine usate per le prove. Usa esempi fittizi.

## Release

Prima di pubblicare una release verifica che la versione in `Cargo.toml`, il tag e i documenti coincidano e che la CI del commit sia riuscita. Compila il binario, esegui le verifiche pertinenti, crea il pacchetto con licenza e guide e calcola i checksum SHA-256 dei file distribuiti.

La prima release contiene il binario gnullvm già verificato localmente. Gli artifact MSVC delle Actions sono compilazioni separate: non sostituirli a un file della release mantenendo il vecchio checksum.
