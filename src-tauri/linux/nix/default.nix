# Call with the signed native archive URL and its verified SHA-256 for this architecture.
{ pkgs, version, archiveUrl, archiveHash }:
pkgs.stdenv.mkDerivation {
  pname = "fetchrail";
  inherit version;
  src = pkgs.fetchurl { url = archiveUrl; hash = archiveHash; };
  sourceRoot = ".";
  nativeBuildInputs = [ pkgs.autoPatchelfHook pkgs.makeWrapper ];
  buildInputs = with pkgs; [ webkitgtk_4_1 gtk3 xdotool libayatana-appindicator stdenv.cc.cc.lib ];
  installPhase = ''
    runHook preInstall
    install -Dm755 fetchrail $out/libexec/fetchrail
    install -Dm644 fetchrail.desktop $out/share/applications/fetchrail.desktop
    install -Dm644 fetchrail.png $out/share/icons/hicolor/128x128/apps/fetchrail.png
    install -Dm644 THIRD_PARTY_NOTICES.txt $out/share/licenses/fetchrail/THIRD_PARTY_NOTICES.txt
    mkdir -p $out/bin
    makeWrapper $out/libexec/fetchrail $out/bin/fetchrail \
      --set FETCHRAIL_LAUNCHER $out/bin/fetchrail \
      --set SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt \
      --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.xdg-utils ]} \
      --prefix XDG_DATA_DIRS : ${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}
    substituteInPlace $out/share/applications/fetchrail.desktop --replace-fail 'Exec=fetchrail' "Exec=$out/bin/fetchrail"
    runHook postInstall
  '';
  meta = { description = "Fetchrail download manager"; platforms = [ "x86_64-linux" "aarch64-linux" ]; mainProgram = "fetchrail"; };
}
