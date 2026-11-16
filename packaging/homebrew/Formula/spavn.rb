class Spavn < Formula
  desc "Terminal-native coding agent that ships as one Rust binary"
  homepage "https://spavn.dev"
  version "0.2.20"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-apple-darwin.tar.gz"
      sha256 "c301c358520793d6ce979282e3a7c79ec7e1b184ff1310125e78e3cccbe8e08f"
    end
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-apple-darwin.tar.gz"
      sha256 "3f908746c1f02aba4bbecb7e50cca6116c1de87955ab54a94d605d4c25c6651b"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4917e2cffd1f9e44ae950f505c13eb0368161a5261122260a6c9641399c01bae"
    end
    # linux-arm region: owned by sync-tap.sh. It emits the block below only when the tag actually
    # published an aarch64-unknown-linux-gnu archive, so no version ever points at a missing file.
    # BEGIN linux-arm
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b869b34567b34ef12698f82ee0984b7b3116c33feaa91d9c79321829ff589b1a"
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
