# Instalar Rust Sentinel Bridge

Windows 10/11 · Chrome, Edge, Brave, Vivaldi o Chromium · **sin permisos de administrador**

<p align="center">

```powershell
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex
```

</p>

Esa es toda la instalación. Pégala en **PowerShell**, responde una pregunta y listo.

* [Qué hace el comando](#qué-hace-el-comando)
* [Paso a paso](#paso-a-paso)
* [Comprobar que funciona](#comprobar-que-funciona)
* [Desinstalar](#desinstalar)
* [Instalación manual o sin internet](#instalación-manual-o-sin-internet)
* [Qué se escribe en el disco](#qué-se-escribe-en-el-disco)
* [Instalación desatendida](#instalación-desatendida)
* [Problemas frecuentes](#problemas-frecuentes)

## Qué hace el comando

`irm` descarga el instalador (`Invoke-RestMethod`) y `| iex` lo ejecuta en la sesión actual
(`Invoke-Expression`). PowerShell no guarda nada en disco y no hay que cambiar la política de
ejecución. Después, el instalador:

| # | Acción | Detalle |
| --- | --- | --- |
| 1 | Descarga el host | `rust-sentinel-bridge.exe` de la release más reciente, no de `main` |
| 2 | Lo verifica | compara el SHA-256 con el `SHA256SUMS.txt` publicado; si no coincide, borra el archivo y se detiene |
| 3 | Lo instala | copia el exe a `%LOCALAPPDATA%\RustSentinelBridge` |
| 4 | Escribe el manifiesto | `com.rustsentinel.bridge.json` con `allowed_origins` fijado a tu id de extensión |
| 5 | Registra el host | claves `HKCU` para Chrome, Edge, Brave, Vivaldi y Chromium (solo los que existan) |
| 6 | Prepara la extensión | si aún no conoce el id, descomprime `rust-sentinel-extension.zip` en `%LOCALAPPDATA%\RustSentinelBridge\extension` y te dice qué carpeta elegir |

Sin servicios, sin tareas programadas, sin reglas de firewall, sin puertos abiertos, sin tocar
el `PATH` y sin reiniciar. El binario solo vive mientras Chrome o Edge mantiene el canal de
native messaging abierto.

## Paso a paso

### 1. Ejecuta el comando

Abre **PowerShell** (Win+X → *Terminal*, o Inicio → *PowerShell*) y pega:

```powershell
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex
```

La salida tiene este aspecto:

```
Rust Sentinel Bridge - installer
  no admin rights, no port, no service - files go to C:\Users\tu\AppData\Local\RustSentinelBridge

  no local binary next to the script, using the latest release
  downloading rust-sentinel-bridge.exe
  sha-256 verified (c1ae6550ef86...) rust-sentinel-bridge.exe
  downloading rust-sentinel-extension.zip
  sha-256 verified (09af9bf29ec8...) rust-sentinel-extension.zip
  extension extracted to C:\Users\tu\AppData\Local\RustSentinelBridge\extension

Step 1 - load the extension (one time only)
  1. Open  chrome://extensions      (Edge: edge://extensions)
  2. Turn on  Developer mode  (top-right switch)
  3. Click  Load unpacked  and select this folder:
       C:\Users\tu\AppData\Local\RustSentinelBridge\extension
     Already loaded from another folder? Keep it and paste its id below.
  4. Copy the 32-character id shown on the new card (letters a-p only)

Paste the extension id (Enter to cancel):
```

### 2. Carga la extensión (una vez, cuatro clics)

Un host de native messaging solo es accesible desde el id de extensión que figura en su
manifiesto, y Chrome crea ese id localmente al cargar una carpeta descomprimida. Por eso este
único paso no se puede automatizar.

1. Abre `chrome://extensions` — o `edge://extensions` en Edge.
2. Activa **Modo de desarrollador** (interruptor arriba a la derecha).
3. Pulsa **Cargar descomprimida** y elige `%LOCALAPPDATA%\RustSentinelBridge\extension`.
4. Copia el id de 32 caracteres de la tarjeta nueva, pégalo en PowerShell y pulsa Enter.

¿Ya usabas la extensión desde otra carpeta (instalación manual anterior)? Consérvala. El
instalador reutiliza `%LOCALAPPDATA%\RustSentinelBridge\extension_id.txt` cuando existe, así
que las actualizaciones no vuelven a preguntar.

### 3. Recarga la extensión

Pulsa la flecha circular de la tarjeta de la extensión. El service worker se reconecta y el
host se lanza bajo demanda.

## Comprobar que funciona

1. Abre la [demo en vivo](https://sovith3.github.io/rust-sentinel-bridge/).
2. El indicador se pone **verde** (*Bridge Connected*) cuando el host Rust responde a un `PING`.
3. Abre la pestaña **League LCU** para ver los eventos de selección de campeón, o el popup
   para un estado rápido.

Si el indicador sigue ámbar, lee `%LOCALAPPDATA%\RustSentinelBridge\bridge.log`: ahí aparece
el motivo (host no registrado para ese id, navegador sin reiniciar, lockfile no encontrado...).

## Desinstalar

```powershell
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/uninstall.ps1 | iex
```

El desinstalador vale para **cualquier** versión, incluidas las anteriores a esta guía:
detiene el host si está corriendo, borra los registros de Chrome/Edge/Brave/Vivaldi/Chromium
que apunten a este proyecto y elimina la carpeta de instalación (y las rutas antiguas, como el
viejo diseño `RustSentinel`). Nunca pide permisos de administrador. Al final, pulsa **Quitar**
en la tarjeta de la extensión en `chrome://extensions`.

Para previsualizar o conservar los logs usa el archivo descargado, porque un comando por tubería
no puede pasar parámetros:

```powershell
powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -DryRun     # solo lista, no toca nada
powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -KeepLog    # guarda bridge.log en el Escritorio
```

## Instalación manual o sin internet

Úsala si PowerShell está restringido, no hay conexión o quieres leer el script antes de
ejecutarlo. Todo viene en el paquete de la release:

1. Descarga `rust-sentinel-extension.zip` y `rust-sentinel-bridge-win64.zip` de la
   [última release](https://github.com/sovith3/rust-sentinel-bridge/releases/latest).
2. Descomprime ambos. Carga la carpeta de la extensión con **Cargar descomprimida** y copia su id.
3. En la carpeta del host ejecuta:

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\install.ps1
   ```

   Pega el id cuando lo pida. El mismo script acepta:

   | Parámetro | Efecto |
   | --- | --- |
   | `-ExtensionId <id>` | evita la pregunta (también vale pegar `chrome-extension://<id>/`) |
   | `-InstallDir <ruta>` | instala en otra carpeta distinta de `%LOCALAPPDATA%\RustSentinelBridge` |
   | `-ExePath <archivo>` | usa un binario que hayas compilado o descargado tú |
   | `-DryRun` | escribe los archivos y el manifiesto, pero no toca `HKCU` |
   | `-NoDownload` | no contacta con GitHub; falla si falta el exe |
   | `-Uninstall` | delega en `uninstall.ps1` cuando está junto al script |

Comprueba cualquier descarga con:

```powershell
Get-FileHash .\rust-sentinel-bridge-*-win64.zip -Algorithm SHA256
```

## Qué se escribe en el disco

Nada fuera de tu perfil de usuario:

| Ruta | Qué es |
| --- | --- |
| `%LOCALAPPDATA%\RustSentinelBridge\rust-sentinel-bridge.exe` | el host de native messaging |
| `%LOCALAPPDATA%\RustSentinelBridge\com.rustsentinel.bridge.json` | manifiesto; `allowed_origins` fija tu id |
| `%LOCALAPPDATA%\RustSentinelBridge\extension_id.txt` | el id de confianza, reutilizado en instalaciones posteriores |
| `%LOCALAPPDATA%\RustSentinelBridge\extension\` | copia descomprimida de la extensión (solo si el instalador tuvo que descargarla) |
| `%LOCALAPPDATA%\RustSentinelBridge\bridge.log` | log del host, se amplía mientras el canal está abierto |
| `HKCU\Software\<navegador>\NativeMessagingHosts\com.rustsentinel.bridge` | un valor por navegador que apunta al manifiesto |

## Instalación desatendida

El comando de una línea no admite parámetros, así que los mismos ajustes existen como
variables de entorno — útil para scripts, imágenes o sesiones remotas:

```powershell
$env:RUST_SENTINEL_EXTENSION_ID  = 'abcdefghijklmnopabcdefghijklmnop'
$env:RUST_SENTINEL_INSTALL_DIR   = 'D:\Tools\RustSentinelBridge'
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex
```

## Problemas frecuentes

| Síntoma | Causa y solución |
| --- | --- |
| `Installation failed: Could not download ...` | Sin internet, un proxy o interceptación TLS corporativa. Usa la instalación manual o define `$env:HTTPS_PROXY` antes de ejecutar el comando. |
| `Installation failed: SHA-256 mismatch ...` | La descarga llegó alterada o cortada; el archivo se borró. Vuelve a ejecutar el comando. |
| `Installation failed: Cannot copy the host binary ...` | Un navegador sigue usando `rust-sentinel-bridge.exe`. Cierra todos los navegadores Chromium y repite. |
| Bucle de `Invalid extension id` | El id tiene 32 caracteres de `a` a `p`. Cópialo de la tarjeta, no del nombre de la carpeta. |
| El indicador sigue ámbar | El manifiesto tiene otro id que la extensión cargada. Reinstala con el id correcto o revisa `bridge.log`. |
| `Specified native messaging host not found` en la consola | Chrome y Edge leen `HKCU` al arrancar: reinicia el navegador después de instalar. |
| Los eventos de League no aparecen | Puede que el socket del LCU esté bloqueado; el host pasa a leer por HTTPS. Define `RUST_SENTINEL_LCU_LOCKFILE` si tu cliente usa una ruta distinta. |

Más detalle: [README en español](../README.es.md) · [README en inglés](../README.md) ·
[protocolo](PROTOCOL.md) · [issues](https://github.com/sovith3/rust-sentinel-bridge/issues)
