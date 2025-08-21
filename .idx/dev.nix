{ pkgs, ... }: {
  channel = "stable-24.05";
  packages = [
    pkgs.gcc
    pkgs.rustup
    pkgs.bun
    pkgs.tree
    pkgs.gnumake
    pkgs.cmake
  ];
  env = { };
  idx = {
    extensions = [
      "pkief.material-icon-theme"
      "tamasfe.even-better-toml"
      "rust-lang.rust-analyzer"
      "bradlc.vscode-tailwindcss"
    ];
    workspace = {
      onCreate = {
        install = "rustup default stable && rustup update && cargo run";
        default.openFiles = [
          "README.md"
        ];
      };
    };
  };
}