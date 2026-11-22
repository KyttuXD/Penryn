# Penryn

A small Minecraft: Java Edition 26.3 client for old Macs: Mac OS X 10.6 Snow Leopard and later,
64-bit Intel, OpenGL 2.1 (a 2008 Mac Pro with a Radeon HD 2600 is the target). Named after that
Mac's CPU family. It plays on 26.3 servers (vanilla or Horizon), with your Microsoft account
through a relay, and draws the game from your own copy of Minecraft.

It is written in Rust with no crates and no standard library (`no_std`), so the same source builds
for Linux, where it is developed and tested, and for Mac OS X 10.6, whose system lacks what
Rust's standard library needs. A Mac build needs no Xcode and no Apple SDK, and is made from Linux.

## What it does

- **Joins 26.3 servers** (protocol 777): offline mode, or online mode signed in with a Microsoft
  account that owns the game. Encryption (AES-128-CFB8, RSA) and compression are built in.
- **Draws the world as vanilla's models**: every block state's own block model (variants,
  multipart, rotations, per-face UVs, cull faces, tints), your game's textures with mipmaps, ambient
  occlusion, water and lava, fog, day and night from the 26.3 world clocks. Chests are vanilla's
  chest model.
- **Vanilla's HUD and screens**, from your game's own sprites and font: hotbar, hearts, food,
  experience, crosshair, held item name, chat (with the game's translations), the F3 screen, the
  inventory with your character in it, chests, dispensers, crafting tables, furnaces, hoppers,
  shulker boxes, tooltips, pause and death screens. In creative, vanilla's creative inventory: every
  tab of the game, scrolling, search (names, tooltips, ids, `#tags`), the survival inventory tab
  with its destroy slot, saved hotbars (C/X + number), and stacks with their components (enchanted
  books, potions, paintings...) sent to the server as vanilla sends them.
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
- **Status effects**: their icons at the top right and beside the inventory (names, times,
  blinking as they run out), jump boost, levitation, slow falling and dolphin's grace on
  movement, blindness and darkness closing the fog in, night vision lifting the night.
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
  prints the frame rate and position each second, and what blocks the player when they walk
  into something. `penryn selftest` also checks movement against gates, ladders, walls, low
  ceilings, ice, soul sand, cobwebs, honey, slime and bamboo.

## Not done yet

- No recipe book. The creative inventory draws potions and tipped arrows untinted and without
  the enchantment glint, and has no drag-splitting of stacks.
- No ambient cave sounds or biome music (music.game everywhere).
- Options screens without anything behind them yet: key binds, language, resource packs,
  telemetry, credits; brightness only lifts the night (Penryn has no light levels yet).
- Block entities other than chests and signs (banners, heads, shulker boxes, bells) are drawn
  without their entity model. Signs show their text (front and back, colours, glow ink) and
  placing one opens vanilla's sign editor; its sign pictures come from `signs.bin` (optional).
- Particles vanilla's blocks make on their own (torches, campfires, leaves, drips) and rain
  splashes are not drawn yet.
- Mobs: no armour trims or dyed leather colours, no heads or pumpkins worn, no shields or other
  specially drawn items in hands, no crouching or swimming player poses, no elytra.
- Only menus listed above have their own screen; others show their slots as chest rows.
- Players' hats, jackets and sleeves always show (the skin customisation bits only hide capes);
  no elytra, and Dinnerbone and Grumm stand the right way up.
- Item stacks with unusual data components (potions, books, banners...) can hide the slots after
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
network and makes those calls for Penryn:

```sh
PENRYN_MSA_CLIENT_ID=<your client id> python3 tools/relay.py      # port 25580
```

It also makes Penryn's game files (`blocks.bin`, `atlas.bin`, `gui.bin`, `entities.bin`, `effects.bin`, `weather.bin`, `signs.bin`, `particles.bin`) from
your own copy of Minecraft: the 26.3 client jar from an existing install (Horizon's launcher, the
official launcher, Prism), or downloaded from Mojang for a signed-in account that owns the game,
plus the data Horizon's `horizon-prepare` writes (install a Horizon release with its launcher).
Penryn fetches them on its first start. To make them by hand:

```sh
python3 tools/make_assets.py ~/.local/share/horizon-launcher/versions/<version>/.local/horizon-client/26.3 \
    --jar ~/.local/share/horizon-launcher/minecraft/versions/26.3/26.3.jar  OUT_DIR
```

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

Copy `dist/Penryn` to the Mac, edit `settings.txt` (the server, the relay, a name for offline
play), double-click **Sign in.command** once, then **Play.command**. Snow Leopard runs 64-bit
programs on its 32-bit kernel, so no special boot is needed.

From a terminal:

```sh
./penryn login RELAY:25580                 # sign in (once)
./penryn --relay RELAY:25580 SERVER:25565  # play, signed in
./penryn --name Steve SERVER:25565         # play offline (offline-mode servers)
./penryn status SERVER                     # the server-list ping
```

`PENRYN_VIEW` (chunks, 2-16; 6 to 8 suit a Radeon HD 2600), `PENRYN_FULLSCREEN=1`, `PENRYN_DATA`
(where the game files and the sign-in are; the program's folder in `Play.command`). Once the
options screens have saved `options.txt` there, its render distance and fullscreen win over
`PENRYN_VIEW` and `PENRYN_FULLSCREEN` (settings.txt's VIEW and FULLSCREEN).

## Controls

Vanilla's defaults: WASD, mouse, space (double-tap to fly in creative), shift sneak, ctrl or double
W sprint, left click dig and attack, right click place and use, 1-9 and the wheel for the hotbar,
E inventory, T chat, / command, F3 debug, F11 full screen, Esc menu.

## Layout

| File | |
|---|---|
| `src/client.rs` | the session: login, configuration, every play packet handled |
| `src/conn.rs`, `net.rs`, `wire.rs`, `zlib.rs`, `crypto.rs`, `md5.rs`, `nbt.rs` | the protocol underneath |
| `src/world.rs`, `blocks.rs` | chunks and the block table |
| `src/render.rs` | meshing and drawing the world (OpenGL 1.5 fixed function) |
| `src/entities.rs` | entities and their models |
| `src/skins.rs` | players' skins and capes: profile textures, PNG decoding, the download thread |
| `src/weather.rs` | rain, snow, thunder and lightning |
| `src/particles.rs` | particles (vanilla's ParticleEngine) |
| `src/gui.rs`, `screens.rs`, `lang.rs` | vanilla's GUI |
| `src/play.rs`, `physics.rs` | the game window, input, movement, digging |
| `src/effects.rs`, `moving.rs` | status effects; pistons and shulker lids in motion |
| `src/sys.rs`, `sdl.rs`, `math.rs` | the C library, SDL2 and OpenGL, float maths (no libm) |
| `src/account.rs`, `http.rs`, `tools/relay.py` | sign-in and the relay |
| `tools/make_assets.py` | the game files, from your copy of Minecraft |
| `scripts/` | the Mac build (VoxelRust's recipe) and packaging |

Packet layouts and ids are Horizon's (`horizon/crates/horizon-protocol`), the authority on 26.3.
The 10.6 build recipe, the inflater and the SDL loading come from VoxelRust (same author, MIT).
