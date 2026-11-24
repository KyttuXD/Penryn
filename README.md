# Penryn

**A Rust Minecraft: Java Edition client that uses [Horizon](https://github.com/KyttuXD/horizon)
for parity, and aims at machines older than Horizon itself can run on.**

Penryn plays Minecraft: Java Edition 26.3 (protocol 777) on real servers, vanilla or Horizon, signed
in with your Microsoft account, drawing the game from your own copy of Minecraft. It is a separate
project, not a fork of Horizon's code: it is a small, self-contained client that takes its 1:1
behaviour from Horizon's Rust work (Horizon's protocol tables and its prepared game data, the
launcher's sign-in) and from the vanilla game's own code, and reimplements it for very old
hardware. Horizon needs macOS 12 and Metal; Penryn's target is a 2008 Mac Pro on **Mac OS X 10.6
Snow Leopard** with a Radeon HD 2600 and OpenGL 2.1, and anything newer (it also runs unchanged on
macOS 12 and on Linux). It is named after that Mac's CPU family.

- **No crates, no standard library.** Penryn is `no_std` with zero dependencies, so one source builds
  for Linux, where it is developed and tested, and for Mac OS X 10.6, whose system lacks what Rust's
  standard library needs. Everything (TLS-free networking, zlib, AES/RSA/SHA, Ogg Vorbis, PNG, NBT,
  the renderer) is written here. A clean release build takes seconds; a Mac build needs no Xcode and
  no Apple SDK, and is made from Linux.
- **Nothing from Mojang is in this repository.** The game files Penryn draws with are made on your
  machine from your own copy of the game (see "The relay").
- **Old systems cannot do modern HTTPS** (10.6 has no TLS 1.2), so a small relay (`tools/relay.py`)
  on any newer computer on your network does the Microsoft sign-in, the session join and other
  downloads, and serves the game files.

## What it does

- **Starts like Minecraft**: vanilla's title screen (the turning panorama, the logo and edition,
  the pulsing yellow splash with its date specials, "Minecraft 26.3", the copyright, Options...,
  Quit Game, the accessibility button) and its multiplayer screens: the server list in
  `servers.dat` (vanilla's own format, so a Minecraft install's file works) with each server's
  ping bars, MOTD in colour, players online with the names in a tooltip, favicon and
  incompatible versions marked; Join Server, double click, Direct Connection, Add Server and
  Edit (name, address, resource packs Enabled / Disabled / Prompt), Delete with its question,
  Refresh, moving servers up and down, and LAN worlds found on the local network. Then
  "Connecting to the server...", "Loading terrain...", the disconnected screen with the
  server's reason, and the pause menu's Disconnect back to the server list. Singleplayer and
  Realms are there but greyed out (Penryn has no integrated server). Its pictures are in
  `menu.bin` (optional); `penryn HOST` still joins a server straight away.
- **Joins 26.3 servers** (protocol 777): offline mode, or online mode signed in with a Microsoft
  account that owns the game. Encryption (AES-128-CFB8, RSA) and compression are built in.
- **Draws the world as vanilla's models**: every block state's own block model (variants,
  multipart, rotations, per-face UVs, cull faces, tints), your game's textures with mipmaps, ambient
  occlusion, water and lava, fog, day and night from the 26.3 world clocks. Chests are vanilla's
  chest model.
- **Vanilla's lighting**: the server's sky and block light (the light half of chunk packets and
  light updates) is stored per section (a section of one level costs one byte), the mesher gives
  every vertex the smooth average of the light around it as vanilla's lighter does, and the
  terrain is drawn with a second texture unit that multiplies in the light map: a 16 x 16 texture
  worked out on the CPU from the 26.x light map shader's arithmetic (the sun's height and the
  weather, the nether's and the end's ambient light, night vision, darkness, the boss bar's
  darkening, the brightness option, the torch light's flicker). Entities, dropped items, item
  frames and maps, the held item and hand, particles, sign text and moving pistons are lit by the
  light at their block (the entity's eye, or the particle's cell) with the same table, so a
  zombie in a dark cave is dark and one beside a torch is warm. There is no separate night
  overlay and no global daylight scalar on the world: the sky, stars and clouds keep their own.
- **Fast on old cards, the same picture**: vanilla's cave culling (each section's visibility set
  and the walk from the camera through them), whole sections of faces turned away from the camera
  never sent, sections packed into shared vertex buffers and drawn near to far with one call per
  16384 quads, meshing on worker threads so loading chunks does not stall frames, clouds and stars
  kept on the card, and vanilla's Cutout Leaves option (off: opaque leaves that hide each other).
- **Vanilla's HUD and screens**, from your game's own sprites and font: hotbar, hearts, food,
  experience, crosshair, held item name, chat (with the game's translations), the F3 screen, the
  inventory with your character in it, chests, dispensers, crafting tables, furnaces, hoppers,
  shulker boxes, tooltips, pause and death screens. In creative, vanilla's creative inventory: every
  tab of the game, scrolling, search (names, tooltips, ids, `#tags`), the survival inventory tab
  with its destroy slot, saved hotbars (C/X + number), and stacks with their components (enchanted
  books, potions, paintings...) sent to the server as vanilla sends them.
- **Every other container screen** as vanilla lays it out, from `containers.bin` (optional):
  anvils (the name field, the cost), enchanting tables (the three offers in the runes, lapis,
  the clue tooltips), brewing stands (bubbles, fuel), beacons (the powers, confirm), grindstones,
  stonecutters (the server's recipes), looms (the patterns), smithing tables (the template
  icons), cartography tables (the map previews), crafters (slot toggles), merchants (the trade
  list, scrolling, the level bar, out of stock), horse, donkey and llama inventories, lecterns,
  and the furnaces' flame and arrow. Books: written books open to read, a book and quill opens
  to write (pages, signing). Maps: the server's map colours and markers, held in one hand or
  both, and in item frames; item frames and glow item frames show what they hold.
- **Plays as vanilla**: walking, sprinting, sneaking (no falling off edges), jumping, swimming,
  creative flight (ends on landing), the 0.6-block step, climbing ladders, vines and scaffolding,
  crouching where standing does not fit, and every block's own collision shape from the game
  (open fence gates and portals let you through, walls and fences stand 1.5 high). Ice and
  slime slide, soul sand and honey slow you, honey halves jumps and lets you slide down its
  sides, slime blocks and beds bounce, cobwebs, berry bushes and powder snow hold you, bubble
  columns lift and pull. Flowers, grass, bamboo and dripstone stand at vanilla's random offset. Digging takes
  vanilla's time for the tool in hand, with the cracks; placing, using items, opening containers
  and moving items (click, right click, shift-click, number keys, Q, drop outside).
- **Mobs, players and dropped items** with their vanilla models and textures, walking animation,
  heads turning, the red flash when hit, falling over when they die. 26.x's baby models, every
  variant (the server's cat, wolf, frog, pig, cow and chicken variants with their climate models,
  horses, llamas, parrots, rabbits, foxes, axolotls, pandas, mooshrooms, tropical fish, villager
  biomes, professions and levels), sheep wool, collars, horse markings, eyes and outer layers,
  armour, saddles, horse and wolf armour, llama carpets, items in hands, and name tags over
  players and named mobs. Left click attacks, right click uses.
- **Players' own skins and capes**: the skin (classic or slim arms, old 64 x 32 skins converted as
  vanilla does) on their model, your first-person arm and the inventory, and capes swinging
  as vanilla's CapeLayer. textures.minecraft.net needs HTTPS, so the relay fetches them
  (`GET /skin?url=...`, cached in its data folder); without a relay, players keep the default
  skins. `PENRYN_TEST_SKIN` (a textures property value, or `@file`) gives players without
  textures that profile, for testing on offline servers.
- **Vanilla's options screens** from the pause menu's Options...: FOV, Online, Skin
  Customization, Music & Sounds (a volume slider per sound category), Video Settings (render
  distance, max framerate, VSync, GUI scale, fullscreen, brightness, clouds...), Controls and Mouse
  Settings (sensitivity, invert), Chat and Accessibility (view bobbing, FOV effects), laid out as
  26.3 lays them out, Esc and Done going back a screen. They are saved in `options.txt` in the data
  folder, with vanilla's key names; options Penryn has no use for yet are shown and kept.
- **Chat as vanilla's, secure chat included**, so Penryn talks on servers that enforce secure
  profiles (the vanilla default): the account's chat key pair (fetched by the relay from the
  services, kept until it expires), the chat session, every message and every `message`
  argument of a command (`/msg`, `/me`, `/say`...) signed in the session's chain (SHA256withRSA,
  written here), the last-seen acknowledgements, and other players' messages checked against
  their keys, with vanilla's "Not Secure" and "Modified" tags and tooltips. Text components in
  full: colours (hex too), bold, italic, underlined, strikethrough, obfuscated, translations
  with their arguments, keybinds, the chat types of the server's registry (`/msg`, `/me`,
  teams), hover tooltips (text, items, entities) and click events in the open chat (commands,
  suggestions, copying, links behind vanilla's confirmation, opened in the system's browser),
  deleted messages, titles, subtitles and the action bar. The chat box has vanilla's command
  suggestions from the server's command tree: the list above the input (Tab, arrows), the
  arguments coloured, their usage, parse errors in red, the server's own suggestions; history
  (up and down), cursor keys and the clipboard.
- **What servers show besides the world**, as vanilla's Hud: the tab list (Tab: names and
  display names, team prefixes and colours, spectators greyed, ping bars, faces on online-mode
  servers, the list objective as numbers or hearts, header and footer), the scoreboard sidebar
  (number formats, team-colour sidebars), below-name scores, team name tag visibility, boss bars
  (every colour and notch style, darkened sky and world fog), titles, subtitles and the action
  bar with vanilla's fades, the world border (its wall, the red edge when near, and the player
  stopping at it), the death message and score on the death screen, the immediate respawn rule,
  the pause menu's Server Links (with vanilla's link confirmation), cookies, and transfers to
  another server. Item tooltips show what servers put on stacks: custom names, lore, rarity,
  enchantments and stored enchantments, attribute modifiers (the defaults too), dyed, unbreakable,
  can break and can place on, hidden parts. Every 26.3 data component is read, so containers
  holding unusual stacks arrive whole. Spectators fly through blocks without a HUD and see from
  the entity they spectate; adventure players break only what their item's can_break allows.
  The pictures (boss bars, ping icons, the border wall) are in `serverui.bin` (optional).
- **Riding as vanilla's client**: passengers on their vehicles' seats (each type's passenger
  attachments), the camera riding along, sneak to get off (the server puts you
  where the vehicle lets you off).
  Boats and rafts are rowed on your own client as AbstractBoat does (paddling, turning, water,
  ice and land friction, floating up, the paddles' animation); saddled horses,
  donkeys, mules and camels are steered with the jump bar (the camel's dash and cooldown),
  pigs and striders with their stick (and its boost), the harnessed happy ghast flies along
  your look; the vehicle's hearts take the food bar's place. Minecarts are the server's to
  move. Swimming (sprint under water, the 0.6 box, steering up and down with the look),
  crawling where nothing taller fits, elytra gliding with firework boosts, the riptide
  trident's throw, and other players drawn sitting, crouching, swimming, gliding and sleeping.
- **Status effects**: their icons at the top right and beside the inventory (names, times,
  blinking as they run out), jump boost, levitation, slow falling and dolphin's grace on
  movement, blindness and darkness closing the fog in, night vision lifting the light map.
- **Pistons and shulker boxes move**: pistons push and pull (slime and honey included) as
  vanilla's client runs them, the moving blocks drawn sliding, carrying and launching the
  player; an opening shulker box lifts what stands on it.
- **Particles as vanilla's ParticleEngine**: blocks breaking into pieces of their texture (tinted
  as the block is), the cracks flying off the face being dug, sprinting and eating crumbs, and
  what the server sends (smoke, flames, hearts, crits, dust, portals, notes, splashes, campfire
  smoke, explosions with their block debris, death poofs and the rest) with the game's own
  sprites from `particles.bin`, each moving as its vanilla class does. At most 4000 at once.
- **Sky as vanilla**: sun, moon phases, stars, fancy clouds, the sky and fog colours of each
  biome, and grass, leaves and water in their biome colours (blended).
- **Weather as vanilla**: rain and snow falling around you where each biome rains or snows
  (colder high up), the sky, fog and clouds greying under rain and thunder, rain sounds on the
  ground or a roof, lightning bolts with their thunder and the sky's flash. The textures and
  biome climates are in `weather.bin` (optional).
- **Sound and music from your game**: every sound the server plays, plus the ones vanilla plays
  itself (footsteps, digging, breaking and placing blocks, buttons) with vanilla's volumes, pitch,
  distance fade and stereo, and the game's music every 10 to 20 minutes. sounds.bin (about
  375 MB) is fetched from the relay once; without it the game plays silent. `PENRYN_PERF=1`
  prints the frame rate and position each second, where each frame's time goes (events,
  network, sound, ticks, meshing, sky, world, entities, block entities, clouds, particles,
  water, weather, hand, GUI, swap), the stage behind the slowest frame, and what blocks the
  player when they walk into something; `PENRYN_PERF=2` finishes the GL queue after each
  stage to charge it its GPU time, `PENRYN_PERF=gpu` measures each stage on the card with timer
  queries, and `PENRYN_MESH_THREADS=0` meshes on the main thread. `penryn scene OUT.png [BLOCK... | id:N]`
  draws blocks offline with the game's renderer (a fixed camera; `PENRYN_SCENE="x,y,z,yaw,pitch"`
  moves it), for checking block textures without a server. `penryn selftest` also checks movement against gates, ladders, walls, low
  ceilings, ice, soul sand, cobwebs, honey, slime and bamboo.

- **Resource packs, the server's and your own**, as vanilla's pack stack: the server's pack
  (pushed and popped in configuration or play) with vanilla's confirm screen (its prompt text;
  declining a required pack leaves the server), the server list's Enabled / Disabled / Prompt
  setting in `servers.dat`, and the statuses the server sees (accepted, downloaded, loaded,
  declined, failed download or reload, discarded). Options > Resource Packs chooses packs from
  the relay's `resourcepacks/` folder (saved as `resourcePacks` in `options.txt`). The relay
  downloads the pack (HTTP or HTTPS, SHA-1 checked, at most 250 MB, kept by its hash) and bakes
  the stack into Penryn's game files with `make_assets.py --packs`: block and item textures and
  models, 26.x item definitions (`items.bin`: the stacks with an `item_model` or a `custom_model_data` string draw their model in slots, the hotbar, the hand, drops and item frames; not yet in the creative tabs or on the cursor), block states, GUI sprites (nine-slice), fonts (bitmap
  providers), language, `sounds.json` and sounds (a small `packsounds.bin` over sounds.bin),
  particles, entity textures, sun, moon and rain, animated block and model textures (`animations.bin`, anim.rs: frametime, frame lists, interpolate; GUI, entity and particle sheets keep their first frame),
  `pack.mcmeta` formats, overlays and filters; higher-resolution textures are scaled to the
  game's size. Only the files the packs change are fetched, and they are loaded at once, without
  reconnecting. Core shaders are ignored (OpenGL 1.5).

## Not done yet

- Menus: no Singleplayer, Realms, friends list, language or credits screens; server addresses
  are not looked up as SRV records.
- Resource packs: no pack icons, drag and drop or pack folder button in the selection screen;
  TrueType fonts, pack panoramas and splashes on the title screen, GUI, entity and particle
  animations past their first frame, and core shaders are not used (bitmap, space and unihex fonts are: the
  packs' other fonts such as `minecraft:gui` draw too, and Unifont's symbols come from a sheet of
  their own).
- Chat: text in the alt font is drawn with the default glyphs; other players'
  chat keys are not checked against the services' key list; no chat delay, narrator,
  chat width/opacity options or saved command history yet.
- No recipe book. The creative inventory draws potions and tipped arrows untinted and without
  the enchantment glint, and has no drag-splitting of stacks.
- No ambient cave sounds or biome music (music.game everywhere).
- Options screens without anything behind them yet: language,
  telemetry, credits.
- Light: the End's flash and the conduit's water vision are not in the light map, servers' custom
  dimension light attributes are not read (only the three vanilla dimensions), light-emitting
  models are not drawn at full light (a torch's flame, glowstone) and light is not smoothed per
  face the way vanilla's `ambientocclusion: false` models are, block faces next to light
  sources are lit by the server's light only.
- Block entities other than chests and signs (banners, heads, shulker boxes, bells) are drawn
  without their entity model. Signs show their text (front and back, colours, glow ink) and
  placing one opens vanilla's sign editor; its sign pictures come from `signs.bin` (optional).
- Particles vanilla's blocks make on their own (torches, campfires, leaves, drips) and rain
  splashes are not drawn yet.
- Mobs: no armour trims or dyed leather colours, no heads or pumpkins worn, no shields or other
  specially drawn items in hands (a shield's pose is drawn); a player's loaded crossbow is held
  as an unloaded one (the server does not send its charge), and a crossbow being charged is
  drawn half way.
- No enchanting book model, smithing armour stand or mount model in their screens; the
  stonecutter does not resolve recipe inputs given as tags; a held map is drawn without the arms
  holding it.
- Riding: striders do not walk on lava yet, no third-person view, a horse's rearing and
  the camel's sitting transitions are not followed on the seat.
- Players' hats, jackets and sleeves always show (the skin customisation bits only hide capes);
  and Dinnerbone and Grumm stand the right way up.
- Tooltips have no potion effects, book authors, firework or banner lines for stacks whose
  components the server changed; no advanced tooltips
  (F3+H). The spectator hotbar menu (teleporting to players) and seeing invisible entities as
  spectator are not done; can_break and can_place_on check blocks and tags, not block states.
- Item stacks with unusual data components (some potions and other rare ones) can hide the slots after
  them in a container until the server sends them again.

## Building (on Linux)

Needs Rust with the `x86_64-apple-darwin` target and Python 3.

```sh
rustup target add x86_64-apple-darwin
scripts/build-mac.sh          # dist/mac/penryn
scripts/package-mac.sh        # dist/Penryn: the folder to copy to the Mac
cargo build --release         # a Linux build, for testing
```

`scripts/package-mac.sh` takes SDL2 from `../../VoxelRust/vendor/sdl2/SDL2.framework` (SDL 2.0.4,
which runs on 10.6), or from a path you give it.

## The relay

Mac OS X 10.6 cannot make the TLS 1.2 connections Microsoft, Xbox Live and Mojang require. The
relay, `tools/relay.py` (Python 3, standard library only), runs on a newer computer on the same
network and makes those calls for Penryn (sign-in, the session join, the chat key pair for
secure chat: `POST /certificates`):

```sh
PENRYN_MSA_CLIENT_ID=<your client id> python3 tools/relay.py      # port 25580
```

It also makes Penryn's game files (`blocks.bin`, `atlas.bin`, `gui.bin`, `entities.bin`, `effects.bin`, `weather.bin`, `signs.bin`, `particles.bin`, `menu.bin`, `serverui.bin`, `containers.bin`) from
your own copy of Minecraft: the 26.3 client jar from an existing install (Horizon's launcher, the
official launcher, Prism), or downloaded from Mojang for a signed-in account that owns the game,
plus the data Horizon's `horizon-prepare` writes (install a Horizon release with its launcher).
Penryn fetches them on its first start. To make them by hand:

```sh
python3 tools/make_assets.py ~/.local/share/horizon-launcher/versions/<version>/.local/horizon-client/26.3 \
    --jar ~/.local/share/horizon-launcher/minecraft/versions/26.3/26.3.jar  OUT_DIR
```

Resource packs go through the relay too: it downloads a server's pack and bakes the pack stack
into the files the packs change (kept in DATA/packs); your own packs go in DATA/resourcepacks
(`~/.local/share/penryn-relay/resourcepacks` by default), zips or folders as vanilla takes them.

Nothing from Mojang is in this repository.

Tokens travel unencrypted on your network between the Mac and the relay: run it only on a network
you trust.

### Microsoft sign-in: your own app registration

Signing in needs an Azure app registration of your own; never use another application's client id.

1. At https://portal.azure.com → Microsoft Entra ID → App registrations → New registration: any
   name, **Personal Microsoft accounts only**, no redirect URI. Then Authentication → **Allow
   public client flows: Yes**. Copy the **Application (client) ID**.
2. Ask Mojang to allow it on the Minecraft API: the form at https://aka.ms/mce-reviewappid
   ("App ID review"). Until it is approved, sign-in stops with "this app is not approved by Mojang
   yet". Offline-mode servers work without any of this.

## On the Mac

Copy `dist/Penryn` to the Mac, edit `settings.txt` (the relay, a name for offline play, and
optionally a server, which is put in the Multiplayer list), then open Penryn: it starts at the
title screen. Its **Sign in...** button signs in through the relay (Horizon's launcher there, or
Microsoft's code shown in the window); **Sign in.command** does the same in Terminal. Snow
Leopard runs 64-bit programs on its 32-bit kernel, so no special boot is needed.

From a terminal:

```sh
./penryn --relay RELAY:25580               # the title screen
./penryn login RELAY:25580                 # sign in (once)
./penryn --relay RELAY:25580 SERVER:25565  # play, signed in
./penryn add-server SERVER:25565 [NAME]    # put a server in the Multiplayer list
./penryn --name Steve SERVER:25565         # play offline (offline-mode servers)
./penryn status SERVER                     # the server-list ping
```

A hostname on the default port 25565 is looked up for its `_minecraft._tcp` SRV record first (as the
game does), over UDP to the first nameserver in `/etc/resolv.conf`; `PENRYN_DNS=IP:PORT` names
another one (for tests). Without a record the name is joined on port 25565.

`PENRYN_VIEW` (chunks, 2-16; 6 to 8 suit a Radeon HD 2600), `PENRYN_FULLSCREEN=1`, `PENRYN_DATA`
(where the game files and the sign-in are; the program's folder in `Play.command`). Once the
options screens have saved `options.txt` there, its render distance and fullscreen win over
`PENRYN_VIEW` and `PENRYN_FULLSCREEN` (settings.txt's VIEW and FULLSCREEN).

## Offline, no relay

The relay is optional. Without one Penryn plays offline-mode servers under a name: `penryn --offline [--name NAME]
HOST[:PORT]` (or `PENRYN_OFFLINE=1`; without a server it is the title screen). The title screen's **Change name...**
button sets the name (1 to 16 letters, digits and `_`) and keeps it in `offline-name.txt` in the data folder; without
`--name` the saved name is used, else the computer's user name. Offline it never contacts a relay: no Microsoft
sign-in (so no online-mode servers), no skins, no server resource packs, no update check. The Mac app is offline when
`RELAY=` is empty in its `settings.txt` (the default). Singleplayer is not part of Penryn: the title screen's
Singleplayer button stays greyed.

The game files are read from `PENRYN_DATA`, else (Linux) the current folder when it holds `blocks.bin`, else
`~/.local/share/penryn`. They are made once, from this computer's own Minecraft 26.3 and Horizon's prepared data, with
no server running:

```sh
python3 tools/relay.py --build-assets ~/.local/share/penryn
penryn --offline localhost:25565
```

## High-resolution textures

Textures larger than 16 px (a 32x or 64x pack's blocks, vanilla's 32 px shelves and signs, and above all ModelEngine's
one 64 to 512 px sheet per model) keep their size: `make_assets.py` places them below the 16x16 tiles in the atlas
(`atlas.bin` grows to 1024 x 2048..8192) and bakes the model faces that use them with their own UVs. The area is
limited (7 million pixels): when a pack has more, the longer side of its big textures is cut by halves (never below 16),
other packs' custom items first, then ModelEngine's models, vanilla last. Flat sprite icons (generated items) and
particles still use one 16x16 tile each. A GPU whose largest texture is smaller than the atlas height cannot show a
big pack.

## Display entities

`block_display` and `item_display` entities are drawn with the transformation the server sends (translation, scale,
both rotations, glided over the server's duration, and the billboard setting): holograms and plugins such as
JoltRagdolls (ragdolls are item displays of player heads, shown with the skin from the head's profile, through the
relay). Items drawn are flat items, block items and player heads; other mob heads and `text_display` are not drawn yet.

## Updates (nightly builds)

CI builds Penryn on every push to `main` and replaces the **nightly** [release](../../releases/tag/nightly)
(`penryn-macos-x86_64.zip`, `penryn-linux-x86_64.tar.gz`). The nightly builds update themselves: at
start, before the window opens, Penryn asks the relay for the release's SHA-256 (the relay reads the
release from GitHub, which Penryn cannot do itself, checks the download against the checksum GitHub
reports, and hands the files over). When it differs from the one installed, the files are put in place
(the Mac app in place, the Linux program and the files beside it) and Penryn starts again. Needs the
relay's `tools/relay.py` of the same download (restart it after an update: it is not replaced while it
runs). Off with `UPDATE=no` in the Mac app's `settings.txt`, `--no-update`, or `PENRYN_NO_UPDATE=1`.
The relay is what Penryn trusts for this (as it already is for your sign-in): the checksum is checked there, on
your computer, not in the program, so keep the relay on a network you trust. A build made by hand (not by CI) never checks, so it is never replaced. The installed release is
recorded in `installed-release.txt` in the data folder; a release that is installed twice without
becoming the running program is not installed a third time.

## Voice chat (off by default)

Penryn speaks the Simple Voice Chat server mod (2.6.x, compatibility 20): Options > Voice Chat turns
it on (`voicechat_enabled` in options.txt), chooses push to talk (key V, rebindable) or voice
activation, the activation level and the volume. Nothing is sent and nothing is queued while it is
off. Speech is Opus (mono, 48 kHz, CELT, 32 kbit/s) over AES-GCM on the mod's UDP port; others' voices
are heard with the mod's distance fall-off and stereo pan. The codec is Penryn's own port of libopus in
`src/opus/` (decoder: SILK, CELT, hybrid; encoder: CELT only), checked against libopus itself; the
protocol layer is `src/voicechat/`. The microphone is SDL's on Linux and AudioQueue (loaded at run
time, so 10.5 and newer) on the Mac. Test hooks: `PENRYN_VOICE_TONE=1` speaks a tone,
`PENRYN_VOICE_LEVEL=1` logs the microphone level.

## Windows

The nightly release has `penryn-windows-x86_64.zip` and `penryn-windows-arm64.zip` (Windows 10 or newer). Unzip the
folder anywhere and run `penryn.exe` from a terminal, or double-click `Penryn-offline.bat` (`penryn.exe --offline`). The
zip holds `SDL2.dll` (Penryn loads it at run time from the folder of `penryn.exe`; the ARM64 one is built by CI from the
SDL2 2.32.10 source, because SDL publishes none), the relay and game-file maker in `tools/` (they need Python 3), and
needs no Visual C++ Redistributable (the C runtime is inside `penryn.exe`). The console window stays open on purpose, so
the log is there.

- **Data folder:** `PENRYN_DATA`, else the current folder when it holds `blocks.bin`, else `%APPDATA%\Penryn` (made when
  needed). `account.txt`, `servers.dat`, `options.txt` and the game files live there. The game files are made once
  from your own Minecraft 26.3 (`py tools\relay.py --build-assets "%APPDATA%\Penryn"`), as in "Offline, no relay".
- **Updates are manual:** there is no self-update on Windows; download the zip from the nightly release again.
- **Server addresses:** the `_minecraft._tcp` SRV record is not looked up on Windows (there is no `resolv.conf`):
  the typed host and port are used. Type the port when a server's SRV record points to another one. (`PENRYN_DNS=ip[:port]`
  turns the lookup on.)
- Names given on the command line and in paths are UTF-8 inside the program, converted at the Windows calls, so a user
  name with accents is fine. The microphone for voice chat is SDL's.

Building on Windows needs [rustup](https://rustup.rs) with the MSVC toolchain (the Visual Studio Build Tools with the
"Desktop development with C++" workload, plus the ARM64 build tools for an ARM64 build; there is no crate to fetch):

```bat
rustup toolchain install 1.99.1
cargo build --release --locked
target\release\penryn.exe selftest
```

Put `SDL2.dll` (the "win32-x64" zip of an [SDL2 release](https://github.com/libsdl-org/SDL/releases/tag/release-2.32.10))
beside `target\release\penryn.exe`. For ARM64 add `--target aarch64-pc-windows-msvc` (after `rustup target add`); build
SDL2 for ARM64 from its source with `cmake -A ARM64`. `.cargo/config.toml` links the C runtime statically.
The Windows code is in `src/sys_win.rs` (the Universal CRT, kernel32 and Winsock under the names `src/sys.rs` uses on
Linux and the Mac); CI is `.github/workflows/build-windows.yml`.

## Controls

Vanilla's defaults: WASD, mouse, space (double-tap to fly in creative), shift sneak, ctrl or double
W sprint, left click dig and attack, right click place and use, 1-9 and the wheel for the hotbar,
E inventory, T chat, / command, F3 debug, F11 full screen, Esc menu.

Every binding is in Options > Controls > Key Binds (vanilla's names in options.txt, `key_key.forward:
key.keyboard.w` and so on); click a button, then press the key or mouse button (Esc unbinds). Q drops
(Ctrl the stack), F swaps with the off hand, middle click picks the block, Tab lists the players,
F1 hides the HUD, F2 saves a screenshot to screenshots/, F5 changes the view (first, third person
behind, third person in front). Sneak and sprint can be toggled, and attack and use too, under
Options > Controls. In third person the player's own model follows the head with the body
(it crouches, swims and turns after the head as vanilla's does). On Linux every run keeps
`~/.local/bin/penryn` linked to the program that is running.

The title screen's top left button signs in, or switches between the accounts Horizon's launcher
has on the relay's computer (and signs in with another Microsoft account or signs out).

## Layout

| File | |
|---|---|
| `src/client.rs` | the session: login, configuration, every play packet handled |
| `src/conn.rs`, `net.rs`, `wire.rs`, `zlib.rs`, `crypto.rs`, `md5.rs`, `nbt.rs` | the protocol underneath |
| `src/world.rs`, `blocks.rs` | chunks and the block table |
| `src/render.rs` | drawing the world (OpenGL 1.5 fixed function): fog, sky, passes, outline |
| `src/anim.rs` | animated textures (`animations.bin`): each game tick the frames step, and the changed ones are written into the atlas with glTexSubImage2D, mips included |
| `src/lightmap.rs` | the light map: the 26.x shader's arithmetic on the CPU, its texture, and how entities, particles and the hand are lit from it |
| `src/mesher.rs`, `terrain.rs`, `occlusion.rs` | meshing sections on worker threads, their vertex buffers and draw calls, cave culling |
| `src/perf.rs` | the per-stage frame timer behind `PENRYN_PERF` |
| `src/opus/` | the Opus codec (a port of libopus: range coder, SILK, CELT, the packet layer, a CELT encoder) |
| `src/voicechat/` | Simple Voice Chat: the protocol, AES-GCM, the session state machine, the UDP socket, the Mac microphone, the glue |
| `docs/parity-sweep.md` | what Penryn still lacks from Minecraft 26.3, from an audit against the client jar |
| `src/entities.rs` | entities and their models |
| `src/mobs.rs`, `signs.rs`, `font.rs`, `creative.rs`, `options.rs` | mob variants, sign pictures and text, the font, the creative inventory, the options screens |
| `src/skins.rs` | players' skins and capes: profile textures, PNG decoding, the download thread |
| `src/packs.rs` | resource packs: the server's pushes, the player's own packs (options `resourcePacks`), the selection screen, the statuses sent back |
| `src/title.rs`, `multiplayer.rs`, `servers.rs` | the title screen and panorama, the multiplayer screens, servers.dat, the server-list ping, LAN worlds |
| `src/task.rs`, `json.rs` | work on threads of its own (pings, joining, signing in); a JSON reader |
| `src/weather.rs` | rain, snow, thunder and lightning |
| `src/particles.rs` | particles (vanilla's ParticleEngine) |
| `src/gui.rs`, `screens.rs`, `lang.rs` | vanilla's GUI |
| `src/itemmodels.rs` | the packs' item definitions (`items.bin`): the model of a stack with an `item_model` or a `custom_model_data` string |
| `src/talk.rs`, `chatui.rs`, `commands.rs`, `text.rs` | secure chat and its keys, the chat screen, the command tree, text components |
| `src/menus.rs`, `books.rs`, `maps.rs` | the other container screens, books, maps and item frames |
| `src/play.rs`, `physics.rs` | the game window, input, movement, digging |
| `src/effects.rs`, `moving.rs` | status effects; pistons and shulker lids in motion |
| `src/riding.rs`, `boat.rs`, `special.rs`, `poses.rs` | vehicles and their seats, boats, swimming, crawling, gliding and players' poses |
| `src/mobanim.rs` | the limbs of mobs and players beyond the walk: bows, crossbows, shields, zombies' and skeletons' arms, piglins, spiders, chickens, elytra wings |
| `src/serverui.rs`, `serverhud.rs` | tab list, scoreboard, teams, boss bars, titles, world border, links, cookies, transfers |
| `src/tooltip.rs`, `gamemode.rs` | item components and tooltips; spectator and adventure mode |
| `src/main.rs` | the command line, and the commands that run without a window (`status`, `join`, `selftest`, `scene`) |
| `src/sys.rs`, `sdl.rs`, `math.rs` | the C library, SDL2 and OpenGL, float maths (no libm) |
| `src/account.rs`, `http.rs`, `tools/relay.py` | sign-in and the relay |
| `tools/make_assets.py` | the game files, from your copy of Minecraft |
| `scripts/` | the Mac build (VoxelRust's recipe) and packaging |

Packet layouts and ids are Horizon's (`horizon/crates/horizon-protocol`), the authority on 26.3.
The 10.6 build recipe, the inflater and the SDL loading come from VoxelRust (same author, MIT).
