# Claude Account Switcher

> **Fair warning: this thing is entirely vibe coded.**
>
> Not one line of it was typed by hand. It was prompted into existence with
> Claude Code, and reviewed mostly by using it. It works, though — so, honestly:
> who cares.

App desktop (Tauri 2 + React + React Aria Components) per tenere salvati più
account Claude Code su Linux e macOS e cambiare quello attivo dalla finestra o
dall'icona nella barra di sistema.

L'interfaccia è in italiano e in inglese: segue la lingua di sistema, con un
override nelle impostazioni.

## Come funziona

Claude Code tiene il login in due pezzi:

- i **token OAuth** — su Linux in `~/.claude/.credentials.json`, su macOS nel
  portachiavi di accesso, sotto il servizio `Claude Code-credentials`
- `~/.claude.json` — configurazione, di cui solo alcune chiavi
  (`oauthAccount`, `userID`, …) identificano l'account

L'app copia entrambi in `~/.config/claude-switch/profiles/<id>/` (su macOS
`~/Library/Application Support/claude-switch/`) e li ripristina quando cambi
account. Il resto di `~/.claude.json` (progetti, cronologia, preferenze) non
viene toccato: la scrittura è atomica e ogni switch lascia uno snapshot in
`backups/`.

Su macOS l'app scrive dove trova il login: se il portachiavi ha già una voce
usa quella, altrimenti ricade sul file — così la CLI legge sempre l'account
giusto, qualunque delle due strade usi la versione installata.

Prima di ogni switch le credenziali vive vengono ricopiate nel profilo attivo,
perché Claude Code ruota i token mentre lavora.

### Consumi e switch automatico

Ogni account mostra due barre: **sessione 5 ore** e **settimana**. I numeri
vengono da `GET https://api.anthropic.com/api/oauth/usage`, lo stesso endpoint
che Claude Code interroga per il suo `/usage`, chiamato con il token OAuth
dell'account. **Non è un'API pubblica**: la sua forma può cambiare senza
preavviso, quindi ogni campo è trattato come opzionale — se manca, la barra
mostra `—` e l'auto-switch non scatta.

Su ogni barra c'è un cursore trascinabile: è la **soglia** oltre la quale
l'account va considerato esaurito (default 100%, cioè solo a limite pieno).
Con lo switch automatico attivo, l'app controlla l'account attivo ogni 3
minuti e, appena **uno dei due** contatori tocca la propria soglia, passa
all'account successivo **nell'ordine della lista** — che riordini trascinando
le righe dalla maniglia a sinistra.

Un candidato già oltre le proprie soglie viene saltato; se nessuno è
disponibile, l'app lo segnala e resta dov'è. Un account di cui non riesce a
leggere i consumi (tipicamente token scaduto) viene invece considerato
utilizzabile: meglio tentare lo switch che restare bloccati su un errore di
rete.

I consumi degli account **non** attivi si leggono con il token salvato: se è
scaduto quelle due barre restano vuote finché non riattivi l'account. Solo
Claude Code rinnova i token, e l'app non lo fa al posto suo.

L'endpoint applica un rate limit, quindi le richieste sono tenute rade: una
risposta resta valida 5 minuti, il controllo periodico gira ogni 5 minuti e
riusa quella cache. Dopo un `429` l'app smette di chiedere per 10 minuti e
continua a mostrare gli ultimi numeri noti anziché svuotare le barre.

La cache sta su disco (`usage.json` accanto ai profili), cooldown compreso.
Tenendola solo in memoria ogni riavvio ripartiva alla cieca e richiedeva tutto
da capo — il modo più sicuro di prendersi un `429` e restare poi dieci minuti
senza numeri su cui decidere, con l'auto-switch fermo.

Il primo controllo parte 15 secondi dopo l'avvio, non dopo cinque minuti; e
anche premere **Aggiorna** lo fa scattare, perché sono gli stessi numeri.

Nel menu della tray ogni account riporta le due percentuali. Quando un
contatore arriva a 5 punti dalla propria soglia compare un `⚠` accanto al nome
e l'icona nel pannello prende un badge di avviso.

### Claude Desktop non è coinvolto

Lo switch vale per la CLI e per l'estensione VSCode, che condividono i file qui
sopra. **Claude Desktop no**: è un'app Electron che si autentica come un
browser, con il cookie `sessionKey` di `.claude.ai` dentro il profilo Chromium
in `~/.config/Claude/`. Nessun file in comune, quindi resta sull'account con
cui l'hai loggato — è una scelta, non un bug.

Volendo si potrebbe estendere lo switch anche a lui (basterebbero i ~200 KB di
`Cookies`, `Local Storage`, `Session Storage`, `IndexedDB`, `Preferences`: il
resto dei 289 MB è cache), ma richiederebbe il Desktop chiuso a ogni cambio ed
è sensibile ai suoi aggiornamenti.

## Prerequisiti

Node 20+ e npm su entrambe le piattaforme.

### Linux

```bash
# toolchain
sudo apt install -y build-essential curl file libssl-dev pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# runtime Tauri 2 + tray su Ubuntu 24.04+
sudo apt install -y libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  libayatana-appindicator3-dev
```

**GNOME**: la barra in alto non mostra le tray icon senza l'estensione
[AppIndicator and KStatusNotifierItem Support](https://extensions.gnome.org/extension/615/appindicator-support/).
Senza quella, l'app funziona ma vedi solo la finestra.

**Scaling frazionario**: GTK 3 non lo sa fare su Wayland. Con
`scale-monitor-framebuffer` attivo il compositor e il toolkit non concordano su
quanto è grande la finestra: i click finiscono accanto ai pulsanti invece che
sopra, e trascinandola su un monitor con scala diversa WebKitGTK ridipinge in un
buffer della taglia vecchia — la finestra torna mezza disegnata. Su una sessione
così l'app passa da sé a XWayland, che scala lui. Per tenere il backend nativo:
`CLAUDE_SWITCH_KEEP_WAYLAND=1`.

**NVIDIA**: il renderer DMA-BUF di WebKitGTK e il driver proprietario non vanno
d'accordo. Dove il modulo `nvidia` è caricato l'app imposta da sé
`WEBKIT_DISABLE_DMABUF_RENDERER=1`, a meno che tu non l'abbia già impostata.

### macOS

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Niente altro: WebKit e la barra dei menu sono di sistema. L'icona nella barra
usa il glifo come *template*, quindi segue tema chiaro e scuro.

## Sviluppo

```bash
npm install
npm run app        # tauri dev
npm run app:build  # .deb / AppImage / .rpm su Linux, .app / .dmg su macOS,
                   # in src-tauri/target/release/bundle
```

Due script accessori:

```bash
python3 scripts/generate-icons.py     # rigenera le icone di tray e app
./scripts/install-desktop-entry.sh    # icona nel dock durante lo sviluppo
```

`generate-icons.py` disegna tutto senza librerie grafiche. Il glifo delle frecce
è `arrows-exchange` di Tabler, ridisegnato dai suoi path: nella tray in
terracotta su fondo trasparente, nell'icona dell'app in bianco e inclinato di
15° al centro di una squircle terracotta. Non c'è niente di preso da altri
marchi, quindi lo script gira su qualsiasi macchina.

`install-desktop-entry.sh` è solo per Linux e solo in sviluppo: `tauri dev`
lancia un binario nudo e GNOME, senza un `.desktop` corrispondente, mostra
un'icona generica. Il pacchetto costruito con `app:build` porta il proprio e non
ne ha bisogno.

Se `npm run app` fallisce con *OS file watch limit reached*, i watch inotify del
sistema sono esauriti (Dropbox e simili ne consumano decine di migliaia):

```bash
echo 'fs.inotify.max_user_watches=524288' | sudo tee /etc/sysctl.d/60-inotify.conf
sudo sysctl --system
```

## Uso

1. Sei già loggato con un account: apri l'app e premi **Salva** nel banner
   "Login non salvato".
2. **Aggiungi account** → disconnetti, apri un terminale, fai `/login` con il
   secondo account, torna e premi **Salva account**.
3. Da qui in poi: clic su un account nella lista, o dal menu dell'icona di
   sistema.

La finestra non ha decorazioni di sistema: la barra in alto è disegnata
dall'app — si trascina da lì, e a destra ci sono impostazioni, riduci a icona e
chiudi. Non c'è un pulsante per ingrandire: una lista di due o tre account non
ci fa niente con uno schermo intero.

Chiudendo la finestra l'app resta attiva nella barra; si esce dal menu
dell'icona. Su macOS un clic sull'icona nel Dock riapre la finestra.

La lingua si cambia in **Impostazioni → Lingua** (automatica, italiano,
inglese) e si applica subito, finestra e menu compresi. Le stringhe stanno in
`locales/*.yml`, condivise fra frontend e backend: per aggiungere una lingua si
copia uno dei due file e si registra il tag in `src/i18n.ts` e
`src-tauri/src/i18n.rs`.

## Avvertenze

- Cambia account con Claude Code **chiuso**: una sessione in corso può
  riscrivere `~/.claude.json` e sovrascrivere lo switch.
- Su Linux i token sono salvati in chiaro (come fa Claude Code stesso) con
  permessi `0600`: non è un keyring. Su macOS i token vivi stanno nel
  portachiavi, ma le copie nei profili dell'app restano file `0600` — quindi
  la stessa avvertenza vale anche lì.
