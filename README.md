# spavn

Terminal tabanlı SPAVN ajanı. Tek Rust binary'si; TUI, oturum yönetimi, araç çağrıları ve yerel/bulut model yönlendirmesi içerir.

## Gereksinimler

- Rust stable
- İsteğe bağlı: yerel model sunucusu (LM Studio, Ollama, llama.cpp — OpenAI uyumlu `/v1` uç noktası)

## Derleme

```sh
cargo build --release
ln -sf "$PWD/target/release/spavn" ~/bin/spavn
```

## Kullanım

```sh
spavn                      # etkileşimli TUI
spavn "tek seferlik soru"  # tek tur, sonuç yazdırır, çıkar
```

Ortam değişkenleri:

| Değişken | Anlamı |
| --- | --- |
| `SPAVN_HOME` | Durum dizini (varsayılan `~/.spavn`) |
| `SPAVN_MODEL` | `local:<ad>` veya `cloud:<ad>` |
| `SPAVN_LOCAL_BASE_URL` | Yerel OpenAI uyumlu uç nokta |
| `SPAVN_SHELL` | `bash -c` için kabuk (yoksa `$SHELL`) |

## Erişim modları

- `SAFE`: her yazma ve kabuk komutu onay ister.
- `AUTO`: geri dönüşü zor komutlar (`rm`, `sudo`, `git reset --hard`) onay ister.
- `SOVEREIGN`: onay istemez; yıkıcı komutlar ve gizli dosyalar yine kapıdan geçer.

`~/.spavn/config.json` içinde `allowed_paths`, `protected_paths` ve model uç noktaları tanımlanır. Oturumlar `~/.spavn/sessions/` altında `0600`, dizinler `0700` izinleriyle tutulur.

## Test

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```
