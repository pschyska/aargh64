{
  description = "aargh64";

  inputs = {
    fup.url = "github:gytis-ivaskevicius/flake-utils-plus";

    devshell.url = "github:numtide/devshell";
    devshell.inputs.nixpkgs.follows = "nixpkgs";
    devshell.inputs.flake-utils.follows = "fup/flake-utils";

    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";

    crate2nix.url = "github:kolloch/crate2nix";
    crate2nix.flake = false;
  };

  outputs = inputs@{ self, nixpkgs, fup, devshell, fenix, crate2nix, ... }:
    fup.lib.mkFlake {
      inherit self inputs;
      supportedSystems = [ "x86_64-linux" ];

      sharedOverlays = [ devshell.overlay fenix.overlay ];

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
          container = package:
            let
              entrypoint = pkgs.writeShellApplication {
                name = "entrypoint.sh";
                runtimeInputs = [ pkgs.coreutils ];
                text = ''
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
          crate2nix-tools = import "${crate2nix}/tools.nix" { inherit pkgs; };
          cargonix = pkgs.callPackage (crate2nix-tools.generatedCargoNix {
            name = "aargh64";
            src = ./rust;
          });
          c2nBuild = release:
            (cargonix {
              inherit release;
              buildRustCrateForPkgs = pkgs:
                (pkgs.buildRustCrate.override {
                  inherit (fenix.packages.${pkgs.system}.minimal) cargo rustc;
                });
            }).rootCrate.build;
            aargh64 = c2nBuild true;
            aargh64-debug = c2nBuild false;
          deploy = pkgs.writeShellApplication {
            name = "deploy";
            runtimeInputs =
              [ certs ensure-kind load pkgs.kubectl pkgs.openssl pkgs.stern ];
            text = ''
              load
              kubectl delete secret admission-controller-tls || :
              kubectl create secret tls admission-controller-tls \
                  --cert "${certs}/cert.crt" \
                  --key "${certs}/key.key"
              kubectl delete --now mutatingwebhookconfiguration aargh64 &>/dev/null || :
              kubectl apply -f "$PRJ_ROOT"/k8s/deployment.yaml
              kubectl apply -f <("${aargh64-debug}/bin/crdgen")
              kubectl apply -f "$PRJ_ROOT"/k8s/po.yaml
              kubectl rollout restart deployment aargh64
              kubectl rollout status deployment aargh64

              CA_PEM64="$(openssl base64 -A <"${certs}"/ca.crt)"
              sed -e s,@CA_PEM_B64@,"$CA_PEM64",g <"$PRJ_ROOT"/k8s/admission_controller.yaml.tpl |
                kubectl apply -f -

              kubectl apply -f "$PRJ_ROOT"/k8s/test.yaml
              kubectl rollout restart deployment test-with-annotation
              kubectl rollout status deployment test-with-annotation
              kubectl rollout restart deployment test
              kubectl rollout status deployment test
              stern -lapp=aargh64
            '';
          };
        in rec {
          packages.aargh64 = c2nBuild true;
          packages.aargh64-debug = c2nBuild false;
          packages.aargh64-docker = container packages.aargh64;
          packages.aargh64-docker-debug = container packages.aargh64-debug;
          defaultPackage = packages.aargh64;
          devShell = pkgs.devshell.mkShell {
            motd = "";
            packages = with pkgs; [
              nix
              bintools
              kind
              stern
              fup-repl
              crate2nix
              build
              ensure-kind
              load
              deploy
            ];
          };
        };
    };
}
