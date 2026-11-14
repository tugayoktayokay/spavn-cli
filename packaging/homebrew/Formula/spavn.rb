class Spavn < Formula
  desc "Terminal-native coding agent that ships as one Rust binary"
  homepage "https://spavn.dev"
  version "0.2.16"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-apple-darwin.tar.gz"
      sha256 "4d1720968c8302a278ef6e712a24fb89ca7c36df1eb6cdb1ffc5b5dcafd94bbf"
    end
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-apple-darwin.tar.gz"
      sha256 "316f077127a5f1cdc17096e02db56ebfc2412c02c9b566aa4e29dbf14c134054"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "57cb46fe3d382e706a5059b2f4a01084d93dd292948b1db4517a4ee8d8eeab85"
    end
    # linux-arm region: owned by sync-tap.sh. It emits the block below only when the tag actually
    # published an aarch64-unknown-linux-gnu archive, so no version ever points at a missing file.
    # BEGIN linux-arm
    on_arm do
      url "https://github.com/tugayoktayokay/spavn-cli/releases/download/v#{version}/spavn-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8a4cadd8e772eb86aa0c75e395aa4799f6deda46f99841a931e69fd6ff2bb157"
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
