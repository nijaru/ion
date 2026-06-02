package session

import (
	"crypto/rand"
	"encoding/hex"
)

// ShortID generates a cryptographically random 8-character hex string.
func ShortID() string {
	b := make([]byte, 4)
	if _, err := rand.Read(b); err != nil {
		return "00000000"
	}
	return hex.EncodeToString(b)
}
