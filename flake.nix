{
  description = "aargh64";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs";

    fup.url = "github:gytis-ivaskevicius/flake-utils-plus";

    devshell.url = "github:numtide/devshell";
    devshell.inputs.nixpkgs.follows = "nixpkgs";
    devshell.inputs.flake-utils.follows = "fup/flake-utils";

    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = inputs@{ self, nixpkgs, fup, devshell, fenix, ... }:
    fup.lib.mkFlake {
      inherit self inputs;
      supportedSystems = [ "x86_64-linux" ];

      sharedOverlays = [
        devshell.overlay
        fenix.overlay
        (final: prev: {

          rustPlatform =
            prev.makeRustPlatform { inherit (prev.fenix.stable) cargo rustc; };
        })
      ];
      outputsBuilder = channels:
        let
          pkgs = channels.nixpkgs;
          certs =
            pkgs.runCommandLocal "certs" { nativeBuildInputs = [ pkgs.openssl ]; } ''
              set -e
              set -u
              set -o pipefail

              mkdir -p "$out"

              printf "subjectAltName = DNS:aargh64.default.svc\n" >admission_extfile.cnf
              openssl req -nodes -new -x509 \
                -keyout ca.key \
                -out ca.crt -subj "/CN=CA"

              openssl genrsa -out "$out"/key.key 2048
              openssl req -new -key "$out"/key.key \
                -subj "/CN=aargh64" |
                openssl x509 -req -CA ca.crt -CAkey ca.key \
                  -CAcreateserial -out "$out"/cert.crt \
                  -extfile admission_extfile.cnf
              cp ca.crt "$out"
            '';
          build = pkgs.writeShellApplication {
            name = "build";
            runtimeInputs = [ pkgs.crate2nix pkgs.nix ];
            text = ''
              env -C "$PRJ_ROOT/rust" crate2nix generate
              nix build "$PRJ_ROOT#aargh64-docker-debug"
            '';
          };
          ensure-kind = pkgs.writeShellApplication {
            name = "ensure-kind";
            runtimeInputs = [ build pkgs.kind pkgs.gnugrep ];
            text = ''
              if ! kind get clusters | grep -xsqF "aargh64"; then
                kind create cluster --name aargh64
              fi
            '';
          };
          load = pkgs.writeShellApplication {
            name = "load";
            runtimeInputs = [ ensure-kind pkgs.kind ];
            text = ''
              ensure-kind
              kind --name aargh64 load image-archive \
                <(zcat \
                  "$(nix build "$PRJ_ROOT"#aargh64-docker-debug \
                    --no-link --print-out-paths)")
            '';
          };
          deploy = pkgs.writeShellApplication {
            name = "deploy";
            runtimeInputs =
              [ certs ensure-kind load pkgs.kubectl pkgs.openssl pkgs.stern ];
            text = ''
              load
              kubectl delete mutatingwebhookconfiguration aargh64 &>/dev/null || :
              kubectl delete deployment aargh64 &>/dev/null || :

              kubectl apply -f "$PRJ_ROOT"/k8s/deployment.yaml
              kubectl rollout status deployment aargh64

              CA_PEM64="$(openssl base64 -A <"${certs}"/ca.crt)"
              sed -e s,@CA_PEM_B64@,"$CA_PEM64",g <"$PRJ_ROOT"/k8s/admission_controller.yaml.tpl |
                kubectl apply -f -

              kubectl apply -f "$PRJ_ROOT"/k8s/test.yaml
              kubectl rollout restart deployment test
              kubectl rollout status deployment test
              stern -lapp=aargh64
            '';
          };
          container = package:
            let
              entrypoint = pkgs.writeShellApplication {
                name = "entrypoint.sh";
                runtimeInputs = [ pkgs.coreutils ];
                text = ''
                  cp ${certs}/cert.crt /admission-controller-tls.crt
                  cp ${certs}/key.key /admission-controller-tls.key
                  install -D "${pkgs.cacert}"/etc/ssl/certs/ca-bundle.crt /etc/ssl/certs/ca-certificates.crt
                  exec ${package}/bin/aargh64
                '';
              };
            in pkgs.dockerTools.buildImage {
              name = "${package.crateName}${if package.release then "" else "-debug"}";
              tag = "latest";
              contents = [ entrypoint ];
              config = { Cmd = [ "${entrypoint}/bin/entrypoint.sh" ]; };
            };
        in rec {
          packages.aargh64 =
            (import ./rust/Cargo.nix { inherit pkgs; }).rootCrate.build;
          defaultPackage = packages.aargh64;
          packages.aargh64-debug = (import ./rust/Cargo.nix {
            inherit pkgs;
            release = false;
          }).rootCrate.build;
          packages.aargh64-docker = container packages.aargh64;
          packages.aargh64-docker-debug = container packages.aargh64-debug;
          devShell = pkgs.devshell.mkShell {
            imports = [ "${devshell}/extra/language/c.nix" ];
            motd = "";
            packages = with pkgs; [
              kind
              stern
              build
              ensure-kind
              load
              deploy
              fup-repl
              pkgs.fenix.stable.toolchain
              crate2nix
            ];

            language.c.includes = [ pkgs.openssl ];
          };
        };
    };
}
