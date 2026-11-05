# Değişiklik Günlüğü

## 2026-11-05 — Sürüm denetimi, crates.io & Homebrew paketleme

### Sürüm denetimi (`update.rs`, `hud/mod.rs`, `slash_commands.rs`, `main.rs`)
- **Otomatik güncelleme uyarısı**: Açılışta `updateCheckIntervalHours` (varsayılan 24 saat) aralığıyla tek bir arka plan `GET` yapılır; yeni sürüm varsa karşılama banner'ında `⬆️ YENİ SÜRÜM VAR` kutusu ve **çalıştırılacak komut** yazılır. Hiçbir şey indirilmez veya çalıştırılmaz.
- **Oturum içi uyarı**: Denetim açılıştan sonra biterse sonuç `PENDING_NOTICE`'a park edilir; giriş döngüsü bir sonraki turunda dock'u silip uyarıyı temiz satıra basar. Aynı sürüm iki kez duyurulmaz (`should_park`).
- **`/update` (`/upgrade`, `/upd`)**: Anında denetim; güncelse `✅`, yeni sürüm varsa komut, hata varsa neden. Denetim kapalıysa "güncel" demek yerine kapalı olduğunu söyler.
- **Kanal algılama**: `/opt/homebrew/Cellar/...` → `brew upgrade spavn`, `~/.cargo/bin` → `cargo install spavn --locked --force`, `~/.local/bin` → `install.sh`. Çalıştırılabilir dosyanın gerçek yolu okunur.
- **Ayarlar**: `updateCheck`, `updateCheckIntervalHours`, `updateCheckUrl` + `SPAVN_UPDATE_CHECK=0`. Sonuç `<spavn_dir>/update-check.json` içinde `0600` olarak önbelleklenir.

### Paketleme
- **crates.io**: `readme`, `homepage`, `documentation`, `keywords`, `categories`, `rust-version = "1.80"` eklendi; `assets/` ve `.github/` paketten çıkarıldı (1.8 MiB → 224 KiB).
- **Homebrew**: `packaging/homebrew/Formula/spavn.rb` — macOS arm64/x86_64 ve Linux x86_64, v0.1.0 SHA-256'ları asset'lerden doğrulanmış, `spvn` symlink'i ve `brew test` bloğu dahil.
- **Release matrisi**: `aarch64-unknown-linux-gnu` eklendi.

### Düzeltmeler
- `web` aracının gömülü arama sağlayıcısı ve gömülü tool adları kaldırıldı; endpoint yalnızca `webSearchUrl` / `SPAVN_WEB_SEARCH_URL`'den gelir.
- `lib.rs` doc listesine `update` modülü eklendi; `--help` çıktısı `/update` ve `SPAVN_UPDATE_CHECK`'i gösterir.

## 2026-11-04 — Apple-tarzı TUI sadeleştirmesi, atomik görsel rozetleri, `/login` merkezi, `subagent`, Windows & yerel Q4 optimizasyonları

### Arayüz & UX (`hud.rs`, `main.rs`, `slash_commands.rs`)
- **Minimalist tek-satır Dock**: Üst ve alt durum çubukları Apple sadeliğinde tek satıra indirildi; varsayılan `local` sunucu ve mükerrer kota/yüzde yazıları temizlendi.
- **Atomik Görsel Rozeti (`[🖼️ Screenshot... (KB)]`) + iTerm2 OSC 1337 Önizleme**: Terminale sürüklenen ekran görüntüleri uzun `/var/folders/...` yolu yerine anında yeşil `[🖼️ dosya.png (KB)]` hapına dönüşür; tek `Backspace` / `Delete` ile tek seferde silinir, `←`/`→` ok tuşlarıyla tek blok olarak atlanır ve gönderildiğinde iTerm2/WezTerm içinde canlı görüntülenir.
- **Canlı Kuyruk Düzenleme & İptal (`↑` / `Ctrl+X`)**: İşlem sürerken (`LiveSpinner`) kuyruğa veya anında müdahaleye (`Steer`) atılan mesajlar giriş satırında önizlenir; `↑` (Yukarı Ok) ile yazım kutusuna geri çekilip düzenlenir, `Ctrl+X` / `Delete` / `/qclear` ile işlemi durdurmadan iptal edilir.
- **Sıfır Titreşimli (DEC Mode 2026) Tam Çerçeveli Menüler**: Tüm menüler (`select_interactive_menu`, `interactive_select_model`) tam kapalı kutu (`╭...╮`, `│...│`, `╰...╯`), dinamik ANSI/emoji sütun hizalaması ve atomik ekran tamponuyla çizilir. Boş `Enter` basışlarında dock aşağı kaymaz.
- **`/login` · `/provider` · `/connect`**: Otomatik CLI token algılama (`gh auth token`, `gcloud auth`), tek tuşla (`o`) tarayıcıdan API anahtar sayfası açma ve yerel model sunucusu bağlama merkezi.

### Yerel LLM & Ajan Çekirdeği (`router.rs`, `tools.rs`, `main.rs`)
- **Yerel Model Örnekleme & XML Kurtarma**: Yerel modellerde sabit `temperature: 0.2` / `max_tokens: 4096` ezmesi kaldırıldı; Qwen3.5/3.6/Ornith `<function=...><parameter=...>` XML tool-call kurtarıcısı, `edit` aracında otomatik girinti (indentation) hizalama (`reindent_replacement_lines`) ve `preserve_thinking` (son 6 mesajda `reasoning_content` koruma) eklendi.
- **`subagent` Aracı & macOS `sandbox-exec` & `--json` / `--version` / `/bench`**: İzole salt-okunur alt-ajan (`subagent`), `SAFE` modda macOS çekirdek seviyesi `sandbox-exec` (Seatbelt) yazma kilidi, `-j`/`--json` çıktı modu, `-V`/`--version` ve saf decode hızını ölçen `/bench` komutu eklendi.
- **Cross-Platform (Windows + Unix)**: Win32 Console (`GetConsoleMode`/`SetConsoleMode`/`SetConsoleScreenBufferInfo`), `SetConsoleCtrlHandler`, `taskkill` ve Microsoft/GitHub Models (`models.inference.ai.azure.com`) + Azure OpenAI desteği eklendi.

## 2026-11-03 — Yerel modeller için kompakt prompt, guard sertleştirme, süreç/yazma dayanıklılığı

### Sistem prompt'u (yerel modeller)
- `build_system_prompt(cwd, compact)`: `endpoint.is_local()` iken kompakt kurallar (6 kural korunur, cümleler kısalır), sembol haritası 900 karakter, profil 500 karakter. Bu repoda 10,8 KB → 5,4 KB (bulut) / ~2,8 KB (yerel, ~710 token).
- Sembol haritası bütçeli (2000 karakter, dosya başına 8 sembol); `PROFILE.md` 1200 karakter; pin ≤ 8, hedef ≤ 5, her biri ≤ 240 karakter.
- `/model`, `/local`, `/byok` ile model değişince sistem prompt'u yeni modele göre yeniden kurulur ve oturuma yazılır.

### Güvenlik (`guard.rs`, `tools.rs`)
- Onay istemi: stdin kapalı/pipe (EOF) artık onay sayılmaz (önceden `rm -rf` sormadan çalışıyordu); Enter tek başına reddeder.
- İç içe kabuk: `bash -c`, `sh -c`, `zsh -lc`, `eval` içindeki komut da denetlenir.
- `cd <korunan> && rm -rf x` göreli yolu `cd` hedefine göre değerlendirir; `${HOME}` tokenizer'da bütün kalır ve ev dizini olarak çözülür.
- `mv` ile korunan/kritik yolu kaynak olarak taşıma silme sayılır; gizli dosya (`~/.ssh` vb.) argümanı onay ister.
- `curl … | sh`, `base64 -d | bash`, `$VAR args` (dinamik komut adı) onay ister.
- `EXA_API_KEY` URL yerine `x-api-key` başlığında.

### Dayanıklılık
- Komut zaman aşımı/kesinti: process group SIGKILL (torun süreçler pipe'ı tutup takılmaz), çıktı 4 MB ile sınırlı, sinyalle ölen süreç `128+sinyal` döner.
- Dosya yazma atomik (geçici dosya + rename; izin ve symlink korunur); SSH yazımında stdin hatası yutulmaz.
- Retry: durum kodu ayrıştırma (408/425/429/500/502/503/504/529), `Retry-After` (≤ 30 sn) saygısı, 3 deneme; yalnızca akış başlamadan önceki bağlantı hataları yeniden denenir.
- Terminal: `TermiosGuard` (Drop) ham modu panikte/atlanan çıkışta da geri yükler.
- Tur kurtarma: `is_premature_stop` ayrı fonksiyon; "let me know" ile biten nihai cevaplar artık gereksiz ek tur tetiklemez.

### Testler (53 → 69)
- Guard (iç içe kabuk, cd, mv, `${HOME}`, pipe-to-shell), atomik yazma, zaman aşımı/sinyal, retry sınıflandırma, prompt bütçeleri, model değişiminde prompt yenileme, markdown dayanıklılık, premature-stop.
- Kalite kapısı: `cargo clippy --all-targets` 0 uyarı, `cargo test` 69/69; mock sunucuyla uçtan uca (tool döngüsü, 429+Retry-After, boş-stop kurtarma, EOF onay reddi, PTY'de ham mod geri yükleme).

## 2026-11-03 — Modüler yapı: `markdown.rs` + `slash_commands.rs` ve test kapsamı

### Mimari
- `src/markdown.rs` (yeni): Tablo motoru (`parse_table_row_cells`, `is_table_separator_row`, `render_markdown_table`) ve akış renderer'ı (`MarkdownStreamRenderer`) `hud.rs`'ten ayrıştırıldı. Escape'li pipe (`\|`) ve inline-code içindeki pipe artık hücre bölmez; boş hücreli satırlar (örn. tek başına `|`) çöp kutu çizdirmemeli düzeltmesi.
- `src/slash_commands.rs` (yeni): Tüm `/komut` dispatch'i (`dispatch_slash_command` → `SlashOutcome::{Handled, ExitLoop, NotSlash}`) `main.rs`'ten ayrıştırıldı. `/reload` artık `-s <session>` ile aktif oturumu devralarak yeniden başlar (önceki `-c` = temiz oturum davranışı terk edildi).

### Testler (39 → 53)
- `spavn_engine.rs`: rol/faz `from_str` alias'ları (TR dahil), `toggled`, `dispatch_role_from_prompt` skor tabanlı yönlendirme, `detect_secrets_in_text` (AWS/GitHub/private-key + yanlış-pozitif koruması), `scrub_secrets` (kısa token dokunulmazlığı, private-key blok kırpma).
- `markdown.rs`: hücre ayrıştırma (escape/code pipe), separator tespiti, boş girdi, separator'sız tablo (ilk satır başlık), eşit olmayan (ragged) satırlar.
- `slash_commands.rs`: `NotSlash` geçişi, `/plan` ↔ `/code` faz senkronu (`ctx.phase` dahil), `/access <mod>` güncellemesi.
- Kalite kapısı: `cargo clippy --all-targets` 0 uyarı, `cargo test` 53/53.

## 2026-11-03 — Native Anthropic Messages API

### `router.rs`: `/v1/messages` protokolü
- `ModelEndpoint::is_anthropic()`: `api.anthropic.com` tabanlı uç noktaları otomatik tanır; `SPAVN_FORCE_ANTHROPIC=1` ile `anthropic.com` dışındaki yerel Anthropic-proxy'lar da native yola alınır. OpenAI-uyumlu Anthropic proxy'leri otomatik olarak eski (OpenAI) yolda kalır.
- `stream_chat_completion` başında dispatch: Anthropic uçları `stream_anthropic`'e yönlendirilir; OpenAI yolu byte-başına değişmez.
- `stream_anthropic`: `x-api-key` + `anthropic-version: 2023-06-01` header'ları, `max_tokens: 16384`, SSE `content_block_delta` (`text_delta`/`input_json_delta`/`thinking_delta`), `content_block_start` (tool_use id+name), `message_delta` (stop_reason) ve `message_start`/`message_delta` (usage) ayrıştırma.
- Mesaj dönüşümü (`split_and_convert_messages`): `system` mesajları tek `system` alanına birleştirilir; `assistant` `tool_calls`'ı `tool_use` bloklarına (girdi JSON olarak ayrıştırılmış), `tool` sonuçları `role:user` + `tool_result` bloklarına çevrilir; ardışık aynı-rol mesajlar birleştirilir (Anthropic rol dönüşümü kuralı).
- Tool şeması: OpenAI `{function:{name,description,parameters}}` → Anthropic `{name,description,input_schema}`.
- `stop_reason` eşlemesi: `end_turn`/`stop_sequence`→`stop`, `tool_use`→`tool_calls`, `max_tokens`→`length`.
- 4 yeni birim testi (system ayrıştırma, tool_result dönüşümü, tool_use girdisi, protokol tespiti). Toplam 38 test.
- Uçtan uca doğrulama (mock SSE): metin akışı, `read` tool_use döngüsü (input_json_delta biriktirme → araç çalıştırma → tool_result geri gönderim → final metin), usage ve stop_reason.

### Doküman
- README: Model routing bölümüne native Anthropic notu; env tablosuna `ANTHROPIC_API_KEY` ve `SPAVN_FORCE_ANTHROPIC`.

## 2026-11-03 — PLAN/CODE fazları ve MIT lisansı

### Faz: PLAN (salt-okunur) / CODE (uygulama)
- Yeni `AgentPhase` (`spavn_engine.rs`): `Plan` / `Code`; `badge`, `short_badge`, `from_str`, `toggled`, `preamble`.
- Zorlama prompt'ta değil tool katmanında: `execute_tool` başında `phase_locked_tool` kontrolü. PLAN'da `write`/`edit`/`bash` (ve tüm takma adları) yapılandırılmış `READ-ONLY PHASE` bildirimi döndürür, dosya asla dokunulmaz. `read`/`search`/`web`/`memory`/`skill` açık.
- `ToolContext.phase` alanı; `new()` imzasına parametre eklendi.
- TUI: status bar'a `📐 PLAN` / `💻 CODE` çipi (access rozetinin yanına); `\` tuşu faz değiştirir (`PromptEvent::CyclePhase`); `/plan`, `/code`, `/act` komutları; faz preamble'ı her turda role preamble'ıyla birlikte enjekte edilir.
- CLI: `-P, --phase plan|code` (script'li salt-okunur plan koşuları için).
- 4 yeni birim testi (kilitleme, açılma, memory/read erişimi, parse/toggle). Toplam 26 test.
- Uçtan uca doğrulama: PLAN'da `write` çağrısında dosya oluşmaz ve oturuma `READ-ONLY PHASE` yazar; CODE'da (varsayılan) `write` çalışır.

### Lisans
- MIT: `LICENSE` eklendi, `Cargo.toml`'a `license`/`authors`/`repository`, `CONTRIBUTING.md`.

## 2026-11-03 — Sertleştirme, taşınabilirlik ve CI

### Güvenlik: yol politikası ve kabuk kapısı
- Yeni `src/guard.rs`: tırnak/kaçış/ayırıcı (`;`, `|`, `&`, `(`, `)`) farkında kabuk tokenizer'ı; `>>`, `2>&1` yönlendirmeleri ve hedef yolları ayrıştırılır.
- `PathPolicy` üç aşamalı yargı: `Allow` / `Ask` / `Deny`. Sıra: araç izni → çalışma alanı kökleri → hassas gizli dizinler (`~/.ssh`, `~/.aws` …) → hassas ev dizinleri → sistem dizinlerine yazma → geçici dizin → dışı.
- `assess_shell_command` risk sınıfları: `Safe`, `Destructive`, `Catastrophic`. `rm`, `find -delete`, `truncate`, `shred`, `dd of=`, `tee`, `sed -i`, `chmod -R`, `mv/cp/ln/rsync`, `git push --force`, `git reset --hard`, `docker`, `systemctl`, `launchctl`, `kill*`, `shutdown/reboot/mkfs`, `diskutil erase`, `defaults`, `sudo` ve yönlendirme hedefleri değerlendirilir.
- Erişim moduna göre davranış: `SAFE` her yazma/kabuk işleminde onay ister; `AUTO` yalnız `Destructive` için; `SOVEREIGN` gizli dosyalar hariç sessiz geçer (`even_sovereign`).
- `tools.rs` içindeki tüm dosya araçları (`read`, `edit`, `write`, `search`, `bash`) tek `enforce_path`/`check_bash_gate` noktasından geçer; eski `has_recursive_rm` sezgisi kaldırıldı.

### Güvenlik: dosya izinleri
- `~/.spavn` 0700, `sessions/` 0700, oturum dosyaları ve `config.json` 0600.
- Mevcut kurulumlar için tek seferlik sıkılaştırma (`harden_sessions_dir`, `secure_file`); yeni oturum dosyaları oluşturulmadan önce izinle açılır.

### Taşınabilirlik
- curl alt süreçlerinin tamamı kaldırıldı; yerine `ureq` 3 + rustls tabanlı `src/http.rs`:
  - `agent_stream()`: bağlantı 10 sn, yanıt başlığı 300 sn, okuma başına 90 sn (`timeout_per_read`), HTTP hataları veri olarak döner.
  - `post_json` / `get_text`: global zaman aşımı, özel başlık desteği.
- SSE akışı ayrı thread'de okunur, `std::sync::mpsc` ile ana döngüye taşınır; ana döngü 150 ms'de bir `is_interrupted()` kontrol eder, Esc/Ctrl+C anında döner.
- Etkilenen çağrılar: `router.rs` sohbet akışı ve yerel model keşfi; `tools.rs` Exa MCP çağrısı ve `web` aracı (User-Agent/Accept korunarak); `spavn_engine.rs` kapı komutları.
- `/bin/zsh` sabitleri kaldırıldı: `config::shell_bin()` sırasıyla `SPAVN_SHELL`, `$SHELL`, `/bin/zsh`, `/bin/bash`, `/bin/sh`.
- `hud.rs` termios katmanı platforma göre ayrıldı: macOS (`u64` alanlar, `NCCS=20`, `TIOCGWINSZ=0x40087468`, `ICANON=0x100`) ve Linux (`u32` alanlar + `c_line`, `NCCS=32`, `TIOCGWINSZ=0x5413`, `ICANON=0x2`).

### Kalite
- `cargo fmt` tüm kod tabanına uygulandı; `cargo fmt --check` temiz.
- Clippy uyarıları 39'dan 0'a indirildi; `cargo clippy --all-targets -- -D warnings` temiz.
- Düzeltmeler: `lines().flatten()` → `map_while(Result::ok)`, `sort_by` → `Reverse` + `sort_by_key`, `min/max` → `clamp`, indeksli döngü → `enumerate`, yinelenen ifade dalları birleştirildi, `&PathBuf` → `&Path`, `too_many_arguments` üç fonksiyonda gerekçeli `allow`.
- Testler: `guard.rs` 21, `session.rs` 1 — toplam 22, hepsi geçiyor.
- Uçtan uca elle doğrulama: sahte SSE sunucusuyla akış ve araç döngüsü (`bash` aracı + `SPAVN_SHELL=/bin/bash`) çalıştırıldı.

### Altyapı
- `git init`, `/target`, `.DS_Store`, `*.rs.bk`, `*.log` dışlayan `.gitignore`.
- GitHub Actions (`ubuntu-latest`, `macos-latest`): `fmt --check`, `clippy -D warnings`, `test --all-targets`, `build --release`.
- `README.md`: derleme, kullanım, ortam değişkenleri, erişim modları, test komutları.

### Bilinen sınırlar
- Linux tarafı yalnız cfg düzeyinde ele alındı; gerçek çalışma CI'da doğrulanacak.
- `http.rs`, `router.rs`, `tools.rs`, `config.rs` için otomatik test yok; SSE doğrulaması elle yapıldı.
- Kabuk kapısı yorumlayıcı dolaylamalarını (`python -c`, `node -e`, `find -exec`, `xargs`) çözmez; symlink TOCTOU açıktır.
- İptalde ureq okuma thread'i en fazla 90 sn arka planda kalabilir; istek iptali ve retry/backoff yoktur.
- Windows desteği yok; `node`, `python3`, `git`, `ssh`, `uptime`, `/bin/date` dış bağımlılıkları sürüyor.
- Binary boyutu rustls/ring ile 1,5 MB → 3,3 MB oldu; strip/LTO eklenmedi.
