class PortZero < Formula
  desc "Developer tool that eliminates port conflicts in local development"
  homepage "https://github.com/PortZeroNetwork/port-zero-local"
  version "0.1.0"
  license "PolyForm-Shield-1.0.0"

  on_macos do
    on_arm do
      url "https://github.com/PortZeroNetwork/port-zero-local/releases/download/v#{version}/port-zero-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # arm64
    end
    on_intel do
      url "https://github.com/PortZeroNetwork/port-zero-local/releases/download/v#{version}/port-zero-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # x86_64
    end
  end

  def install
    bin.install "portzero"
    bin.install "devenv"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/portzero --version")
  end
end
