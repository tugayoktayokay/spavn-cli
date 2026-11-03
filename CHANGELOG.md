# Değişiklik Günlüğü

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
