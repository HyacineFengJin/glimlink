// SPDX-License-Identifier: MIT
// Inject this test into an isolated copy of pinned LagrangeGo's client package.
// It calls the upstream complete password builder, not a rebuilt inner fixture.
// Run with --network=none, cached modules readonly and no account-state mounts.
package client

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"

	"github.com/LagrangeDev/LagrangeGo/client/auth"
)

func TestRuntimeNativeLoginReference(t *testing.T) {
	device := &auth.DeviceInfo{
		GUID: "000102030405060708090a0b0c0d0e0f",
		DeviceName: "synthetic-device",
		KernelVersion: "6.1",
	}
	var password [16]byte
	for i := range password { password[i] = 17 }
	out := map[string]any{
		"source": "LagrangeGo/f7f8abfaa7f1/client/ntlogin.go:buildPasswordLoginRequest",
		"uin": uint32(12345678),
		"widget_app_id": "2086582797",
		"session_id": "synthetic%2Blogin-session",
	}
	for _, version := range []string{"3.2.19-39038", "3.2.26-46494"} {
		for _, withCaptcha := range []bool{false, true} {
			sig := &auth.SigInfo{
				ExchangeKey: make([]byte, 16),
				KeySig: []byte("synthetic-key-signature"),
				Cookies: "synthetic-cookie",
			}
			for i := range sig.ExchangeKey { sig.ExchangeKey[i] = 7 }
			suffix := "/initial"
			if withCaptcha {
				// main.go passes URL sid as the third argument of SubmitCaptcha.
				sig.CaptchaInfo = [3]string{"synthetic-ticket", "synthetic-rand", out["session_id"].(string)}
				suffix = "/captcha"
			}
			frame, err := buildPasswordLoginRequest(12345678, auth.AppList["linux"][version], device, sig, password)
			if err != nil { t.Fatal(err) }
			out[version+suffix] = hex.EncodeToString(frame)
		}
	}
	data, err := json.MarshalIndent(out, "", "  ")
	if err != nil { t.Fatal(err) }
	if err := os.WriteFile(os.Getenv("AUTH_VECTOR_OUT"), append(data, '\n'), 0644); err != nil { t.Fatal(err) }
}
