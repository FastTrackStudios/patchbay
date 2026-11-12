#!/usr/bin/env ruby
# The app's uploads and TestFlight builds as App Store Connect has them,
# newest first: each upload's state with Apple's errors and warnings (an
# upload is listed at once, its build only once processing starts), then
# each build's version, number, processing state and upload time.
# Read-only: it adds no tester or group and changes nothing on the app
# record (apps/studio/desktop/ios/testflight-status.rb, which also wires
# up testers, is not this). architect's apps/mobile/ios/testflight-status.rb.
#
#   ruby apps/patchbay/ios/testflight-status.rb [bundle-id] [build-number]
#
# With a build number it prints that one and exits 1 when App Store
# Connect has neither an upload nor a build of it. `altool --validate-app`
# (deploy-testflight.sh --dry-run) leaves an upload of its number in
# AWAITING_UPLOAD: a reservation, nothing was sent. Reads ASC_KEY_ID,
# ASC_ISSUER_ID and ASC_KEY_PATH from the environment, else from
# ~/.appstoreconnect/config.env.

require "openssl"; require "json"; require "base64"; require "net/http"; require "uri"

def config(name)
  return ENV[name] if ENV[name] && !ENV[name].empty?
  file = File.expand_path("~/.appstoreconnect/config.env")
  line = File.exist?(file) && File.readlines(file).find { |l| l =~ /^\s*(export\s+)?#{name}=/ }
  abort "no #{name} (environment or #{file})" unless line
  line.split("=", 2)[1].strip.delete('"').delete("'").sub("$HOME", Dir.home).sub(/\A~/, Dir.home)
end

BUNDLE_ID = ARGV[0] || "app.fasttrackstudio.patchbay"
BUILD_NO = ARGV[1]
KEY_ID = config("ASC_KEY_ID")
ISSUER_ID = config("ASC_ISSUER_ID")
KEY_PATH = config("ASC_KEY_PATH")

def jwt
  header = { alg: "ES256", kid: KEY_ID, typ: "JWT" }
  now = Time.now.to_i
  payload = { iss: ISSUER_ID, iat: now, exp: now + 600, aud: "appstoreconnect-v1" }
  seg = ->(h) { Base64.urlsafe_encode64(JSON.dump(h), padding: false) }
  input = "#{seg.call(header)}.#{seg.call(payload)}"
  der = OpenSSL::PKey::EC.new(File.read(KEY_PATH)).sign(OpenSSL::Digest.new("SHA256"), input)
  a = OpenSSL::ASN1.decode(der)
  r = a.value[0].value.to_s(2).rjust(32, "\x00")
  s = a.value[1].value.to_s(2).rjust(32, "\x00")
  "#{input}.#{Base64.urlsafe_encode64(r + s, padding: false)}"
end
TOKEN = jwt

def get(path)
  uri = URI("https://api.appstoreconnect.apple.com#{path}")
  req = Net::HTTP::Get.new(uri)
  req["Authorization"] = "Bearer #{TOKEN}"
  res = Net::HTTP.start(uri.host, uri.port, use_ssl: true) { |h| h.request(req) }
  abort "GET #{uri.path}: HTTP #{res.code} #{res.body[0, 300]}" unless res.is_a?(Net::HTTPSuccess)
  JSON.parse(res.body)
end

app = get("/v1/apps?filter[bundleId]=#{URI.encode_www_form_component(BUNDLE_ID)}")["data"].first
abort "no App Store Connect app for #{BUNDLE_ID} (create its record: App Store Connect → Apps → + → New App)" unless app
uploads = get("/v1/apps/#{app["id"]}/buildUploads?limit=10")["data"]
uploads.select! { |u| u.dig("attributes", "cfBundleVersion") == BUILD_NO } if BUILD_NO
puts "#{app.dig("attributes", "name")} (#{BUNDLE_ID})"
puts "uploads:" unless uploads.empty?
uploads.each do |u|
  at = u["attributes"]; st = at["state"] || {}
  puts format("  %-10s build %-12s %-11s uploaded %s", at["cfBundleShortVersionString"], at["cfBundleVersion"],
              st["state"], at["uploadedDate"])
  %w[errors warnings].each do |k|
    (st[k] || []).each { |m| puts "    #{k.chomp("s")}: #{m["code"]} #{m["description"] || m["message"]}" }
  end
end

query = "filter[app]=#{app["id"]}&sort=-uploadedDate&limit=20&include=preReleaseVersion" \
        "&fields[builds]=version,processingState,uploadedDate,expired,usesNonExemptEncryption,minOsVersion,preReleaseVersion" \
        "&fields[preReleaseVersions]=version"
query += "&filter[version]=#{URI.encode_www_form_component(BUILD_NO)}" if BUILD_NO
res = get("/v1/builds?#{query}")
versions = (res["included"] || []).to_h { |v| [v["id"], v.dig("attributes", "version")] }

puts "builds:"
if res["data"].empty?
  puts BUILD_NO ? "  build #{BUILD_NO}: no TestFlight build (yet)" : "  none (yet)"
  exit(BUILD_NO && uploads.empty? ? 1 : 0)
end
res["data"].each do |b|
  at = b["attributes"]
  marketing = versions[b.dig("relationships", "preReleaseVersion", "data", "id")] || "?"
  puts format("  %-10s build %-12s %-11s uploaded %s  min iOS %s  non-exempt encryption: %s%s",
              marketing, at["version"], at["processingState"], at["uploadedDate"], at["minOsVersion"],
              at["usesNonExemptEncryption"].inspect, at["expired"] ? "  (expired)" : "")
end
