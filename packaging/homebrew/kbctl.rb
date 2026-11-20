# Homebrew formula template.
#
# `@VERSION@` and the `@SHA256_*@` placeholders are substituted by
# .github/workflows/homebrew.yaml from the published release assets, and the
# result is pushed to noirbizarre/homebrew-tap as Formula/kbctl.rb.
class Kbctl < Formula
  desc "A unified control plane for programmable keyboards"
  homepage "https://github.com/noirbizarre/kbctl"
  version "@VERSION@"
  license "MIT"

  # The release archive, not the raw executable: it carries the man pages and
  # completions the bare binary cannot. Homebrew strips the archive's single
  # top-level directory, so the paths in `install` start at `bin/`.
  #
  # This project tags without a `v` prefix, so the tag is `#{version}` as-is.

  def install
    bin.install "bin/kbctl"

    # Each is optional: the archive only carries what the project generates.
    man1.install Dir["share/man/man1/*.1"] unless Dir["share/man/man1/*.1"].empty?
    bash_completion.install "share/bash-completion/completions/kbctl" if File.exist?("share/bash-completion/completions/kbctl")
    zsh_completion.install "share/zsh/site-functions/_kbctl" if File.exist?("share/zsh/site-functions/_kbctl")
    fish_completion.install "share/fish/vendor_completions.d/kbctl.fish" if File.exist?("share/fish/vendor_completions.d/kbctl.fish")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/kbctl --version")
  end
end
