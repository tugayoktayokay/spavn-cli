class Spavn < Formula
  desc "Terminal-native coding agent that ships as one Rust binary"
  homepage "https://spavn.dev"
  version "0.2.19"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-apple-darwin.tar.gz"
      sha256 "717a6bae16a9623543aa8f69e61f7f893ad15f6dc256571a0a2ed714575602d2"
    end
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-apple-darwin.tar.gz"
      sha256 "22df34c99479b099f0cbff7d00ce3c85a5d368a399f9bff785b663c1475c2052"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9f0f5dc2e3f5d8b6f41785d710591d160c068648d747e3a479630504a4164251"
    end
    # linux-arm region: owned by sync-tap.sh. It emits the block below only when the tag actually
    # published an aarch64-unknown-linux-gnu archive, so no version ever points at a missing file.
    # BEGIN linux-arm
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "d0dee48a59fd7a9afd04c676d0eea11a3539d76d098c56ff9e466f1a842ae8fc"
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
