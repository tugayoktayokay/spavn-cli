class Spavn < Formula
  desc "Terminal-native coding agent that ships as one Rust binary"
  homepage "https://spavn.dev"
  version "0.2.12"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-apple-darwin.tar.gz"
      sha256 "7c7777f0fe168eefeef95cdc411fdcb8243ba61b08529990a2a76b32ec471471"
    end
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-apple-darwin.tar.gz"
      sha256 "85682bc470b55544adcb8f5ea4b1491836f00277777345210e6fb83e6acf3218"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a8f19e3c75aee24c9dfa0ecbc5e33cf13b3dacedab2dffe0879b963be81bc52b"
    end
    # linux-arm region: owned by sync-tap.sh. It emits the block below only when the tag actually
    # published an aarch64-unknown-linux-gnu archive, so no version ever points at a missing file.
    # BEGIN linux-arm
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "69f20fb5986522d34343dc15c281266ee58f169979dbcdaaa15f1dd3c1037b16"
    end
    # END linux-arm
  end

  livecheck do
    url "https://github.com/tugayoktayokay/spavn-cli/releases/latest"
    strategy :github_latest
  end

  # The archives ship one unstripped-of-anything binary at the root, so `bin.install "spavn"` is
  # exact rather than a glob that could silently pick up a stray file.
  def install
    bin.install "spavn"
    bin.install_symlink "spavn" => "spvn"
  end

  def caveats
    <<~EOS
      Config lives in ~/.spavn (override with SPAVN_HOME). Web search has no
      built-in endpoint: set webSearchUrl or webSearchServer in
      ~/.spavn/config.json to enable it.
    EOS
  end

  test do
    assert_match "spavn v#{version}", shell_output("#{bin}/spavn --version")
    assert_match "Usage: spavn", shell_output("#{bin}/spavn --help")
    assert_predicate bin/"spvn", :symlink?
  end
end
