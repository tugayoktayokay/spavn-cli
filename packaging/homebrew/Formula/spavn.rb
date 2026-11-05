class Spavn < Formula
  desc "Terminal-native coding agent that ships as one Rust binary"
  homepage "https://spavn.dev"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-apple-darwin.tar.gz"
      sha256 "3025e0ed7331525a638d27c57e19f496bf4f034e58005599d426bc764c31f473"
    end
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-apple-darwin.tar.gz"
      sha256 "6757b9bd6d7c42ecbb326365692988eb9f06d5da193c284fc1f6609fbc01938c"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e13501777996c574f1a8dd5c50e2b99a51ae508d1e509797ee7f3a5de776ac94"
    end
    # linux-arm region: owned by sync-tap.sh. It emits the block below only when the tag actually
    # published an aarch64-unknown-linux-gnu archive, so no version ever points at a missing file.
    # BEGIN linux-arm
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
