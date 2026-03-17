#!/usr/bin/env python3
"""Regenerate Formula/a3s.rb with new version and sha256 values."""
import os

ver             = os.environ["VER"]
sha_macos_arm64 = os.environ["SHA_MACOS_ARM64"]
sha_macos_x64   = os.environ["SHA_MACOS_X64"]
sha_linux_arm64 = os.environ["SHA_LINUX_ARM64"]
sha_linux_x64   = os.environ["SHA_LINUX_X64"]

base = f"https://github.com/A3S-Lab/Cli/releases/download/v{ver}"

formula = (
    'class A3s < Formula\n'
    '  desc "Command-line interface for the A3S platform"\n'
    '  homepage "https://github.com/A3S-Lab/Cli"\n'
    f'  version "{ver}"\n'
    '  license "MIT"\n'
    '\n'
    '  on_macos do\n'
    '    on_arm do\n'
    f'      url "{base}/a3s-{ver}-darwin-arm64.tar.gz"\n'
    f'      sha256 "{sha_macos_arm64}"\n'
    '    end\n'
    '    on_intel do\n'
    f'      url "{base}/a3s-{ver}-darwin-x86_64.tar.gz"\n'
    f'      sha256 "{sha_macos_x64}"\n'
    '    end\n'
    '  end\n'
    '\n'
    '  on_linux do\n'
    '    on_arm do\n'
    f'      url "{base}/a3s-{ver}-linux-arm64-musl.tar.gz"\n'
    f'      sha256 "{sha_linux_arm64}"\n'
    '    end\n'
    '    on_intel do\n'
    f'      url "{base}/a3s-{ver}-linux-x86_64-musl.tar.gz"\n'
    f'      sha256 "{sha_linux_x64}"\n'
    '    end\n'
    '  end\n'
    '\n'
    '  def install\n'
    '    bin.install "a3s"\n'
    '  end\n'
    '\n'
    '  test do\n'
    '    assert_match "a3s", shell_output("#{bin}/a3s --version")\n'
    '  end\n'
    'end\n'
)

out = "Formula/a3s.rb"
with open(out, "w") as f:
    f.write(formula)
print(f"Written {out} for v{ver}")
