# Rust Sentinel Bridge

[English](README.md) · **Español**

[![CI](https://github.com/sovith3/rust-sentinel-bridge/actions/workflows/ci.yml/badge.svg)](https://github.com/sovith3/rust-sentinel-bridge/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sovith3/rust-sentinel-bridge?color=38f2b0)](https://github.com/sovith3/rust-sentinel-bridge/releases/latest)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**Conecta cualquier web HTTPS con Ollama local, telemetría de Windows, escaneo de archivos
o el cliente de League of Legends sin abrir puertos TCP, sin servidores locales y sin
errores de CORS.**

![Rust Sentinel Bridge en acción](docs/demo.gif)

**[Demo en vivo](https://sovith3.github.io/rust-sentinel-bridge/)** ·
**[Última release](https://github.com/sovith3/rust-sentinel-bridge/releases/latest)** ·
**[Guía de instalación](docs/INSTALL.es.md)** ·
**[Protocolo](docs/PROTOCOL.md)** ·
**[README completo (inglés)](README.md)**

`rust-sentinel-bridge.exe` es un binario Rust de ~1,5 MB que Chrome o Edge lanzan mediante
**Native Messaging**: stdin/stdout con un prefijo de longitud de 4 bytes. El canal solo
existe mientras el navegador mantiene el puerto abierto, está vinculado a un único id de
extensión y no escucha en la red, así que una página puede llegar a APIs nativas de Windows
que `fetch()` nunca podría tocar.

## ¿Por qué usar esto en lugar de un servidor en localhost?

| | Servidor HTTP en localhost | Rust Sentinel Bridge |
| --- | --- | --- |
| Superficie de red | Un puerto TCP que cualquier proceso local puede escanear y llamar | **Ningún socket**: stdin/stdout propiedad del navegador |
| CORS / preflight | Cabeceras permisivas, un proxy o trucos con `no-cors` | **No aplica**: ninguna petición HTTP sale de la página |
| Páginas HTTPS | El contenido mixto bloquea las llamadas a `http://` | **Permitido**: native messaging no es tráfico de red |
| Quién puede llamarlo | Cualquier página o aplicación que adivine el puerto | **Solo el id de extensión fijado** (`allowed_origins`) |
| Consumo en reposo | Un demonio, un puerto, logs y una entrada de autoarranque | **0 % CPU / 0 MB**: se lanza bajo demanda y muere con el canal |
| Distribución | El usuario instala y confía en un servidor | Un `.exe` y una extensión sin empaquetar, sin permisos de administrador |

## Inicio rápido

### Opción A — Un solo comando (Windows 10/11, sin Rust)

Abre **PowerShell** (sin permisos de administrador) y pega:

```powershell
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex
```

Esa línea descarga el instalador de la última release y lo ejecuta. El instalador verifica el
binario contra el `SHA256SUMS.txt` publicado, lo copia a `%LOCALAPPDATA%\RustSentinelBridge`,
escribe el manifiesto de native messaging y guarda `extension_id.txt`, y por último registra el
host en `HKCU` para Chrome, Edge, Brave, Vivaldi y Chromium. Sin permisos de administrador, sin
tocar el `PATH`, sin servicios y sin puertos abiertos.

Después carga la extensión **una vez**. Solo el navegador puede crear su id de 32 caracteres:

1. Abre `chrome://extensions` (Edge: `edge://extensions`) y activa **Modo de desarrollador**.
2. Pulsa **Cargar descomprimida** y elige `%LOCALAPPDATA%\RustSentinelBridge\extension` — el
   instalador la descomprime ahí cuando todavía no conoce tu id. ¿Ya la tenías cargada desde
   otra carpeta? Consérvala y pega ese id.
3. Si el instalador sigue esperando, pega el id de la tarjeta y pulsa Enter.

Recarga la extensión y abre la [demo en vivo](https://sovith3.github.io/rust-sentinel-bridge/):
el indicador se pone verde cuando el host Rust responde a un `PING`.

**Guía completa paso a paso, problemas frecuentes e instalación desatendida:
[docs/INSTALL.es.md](docs/INSTALL.es.md)** (English: [docs/INSTALL.md](docs/INSTALL.md)).

> ¿Prefieres interfaz gráfica? Ejecutar `rust-sentinel-bridge.exe` una vez sin argumentos hace
> la misma instalación con un cuadro de diálogo; lee el id de
> `%LOCALAPPDATA%\RustSentinelBridge\extension_id.txt` o de `RUST_SENTINEL_EXTENSION_ID`.

Desinstalar en cualquier momento y desde cualquier versión, también en una línea:

```powershell
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/uninstall.ps1 | iex
```

`uninstall.ps1` cierra el host si está corriendo, borra los registros de todos los navegadores
Chromium (diseño actual y antiguos) y elimina las carpetas de instalación, sin permisos de
administrador. Para usar `-DryRun` (ver qué borraría) o `-KeepLog` (guardar `bridge.log`)
ejecuta el archivo descargado en lugar del comando por tubería: un comando así no puede pasar
parámetros.

### Opción B — Instalación manual desde el zip de la release

Úsala si PowerShell está restringido, no hay conexión o quieres leer el script antes de
ejecutarlo.

1. Descarga `rust-sentinel-extension.zip` de la
   [última release](https://github.com/sovith3/rust-sentinel-bridge/releases/latest),
   descomprímela, abre `chrome://extensions`, activa **Modo de desarrollador**, pulsa
   **Cargar descomprimida** y selecciona la carpeta. Copia el id de 32 caracteres.
2. Descarga `rust-sentinel-bridge-win64.zip`, descomprímelo y ejecuta:

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\install.ps1
   ```

   Pega el id cuando lo pida. El mismo script acepta `-ExtensionId <id>`, `-InstallDir <ruta>`,
   `-ExePath <archivo>`, `-DryRun` (no escribe en el registro) y `-NoDownload` (no contacta con
   GitHub).
3. Recarga la extensión y abre la
   [demo en vivo](https://sovith3.github.io/rust-sentinel-bridge/). El indicador se pone verde
   cuando el host responde a un `PING`.

### Opción C — Compilar desde el código fuente

Necesitas Rust estable y las build tools de MSVC (o el fallback GNU documentado en el
[README en inglés](README.md#option-c--build-from-source)).

```bash
git clone https://github.com/sovith3/rust-sentinel-bridge
cd rust-sentinel-bridge/native-host
cargo build --release --locked
```

Desde la raíz del repositorio, registra el host:

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

## Documentación

| Ruta | Qué contiene |
| --- | --- |
| [`README.md`](README.md) | Guía completa: adaptadores, seguridad, API desde la página y desarrollo. |
| [`docs/PROTOCOL.md`](docs/PROTOCOL.md) | Protocolo de tramas, acciones y payloads de cada adaptador. |
| [`docs/INSTALL.es.md`](docs/INSTALL.es.md) | Guía de instalación paso a paso, desinstalación y problemas frecuentes. |
| [`web-demo/`](web-demo/index.html) | Demo estática que usa los cuatro adaptadores desde una página normal. |
| [`install.ps1`](install.ps1) | Instalador y punto de entrada del comando de una línea: descarga y verifica el binario, escribe el manifiesto y registra las claves HKCU. |
| [`uninstall.ps1`](uninstall.ps1) | Desinstalador válido para cualquier versión: limpia registro, carpetas y procesos. |

## Solución rápida: no aparecen los eventos de selección de campeón

El host ya no se rinde. Reintenta la suscripción cada 3 s y, si el socket local está
bloqueado (firewall, antivirus) o el cliente arrancó después, lee el mismo estado por
HTTPS y lo publica igualmente. Abre la pestaña **League LCU** de la demo: se suscribe sola
y el log empieza en cuanto entras en selección. Si tu League está en una ruta rara, define
`RUST_SENTINEL_LCU_LOCKFILE=C:\ruta\al\lockfile`.

## Licencia

Apache-2.0. Consulta [`LICENSE`](LICENSE) y [`NOTICE`](NOTICE).

Rust Sentinel Bridge es un proyecto independiente. No está afiliado, respaldado ni
patrocinado por Riot Games, Microsoft, Google, Brave u Ollama.
