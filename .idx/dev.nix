{ pkgs, ... }: {
  channel = "unstable";
  packages = [
    pkgs.gcc
    pkgs.rustup
    pkgs.bun
    pkgs.gnumake
    pkgs.cmake
    pkgs.flatbuffers
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