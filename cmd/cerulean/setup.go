package main

import (
	"fmt"
	"os"
	"time"

	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
)

// startMachine は run・serve 共通の起動処理。snapLoad があればスナップ
// ショットから再開し（imagePath があれば同一イメージか照合する）、なければ
// imagePath をロードしてリセットする。戻り値の imageID は元イメージの
// SHA-256 で、スナップショットに記録して取り違えの照合に使う。
func startMachine(m *smdk2410.Machine, imagePath, snapLoad, rtcFlag string, nb0Base uint32) (imageID string, err error) {
	if imagePath != "" {
		if imageID, err = fileSHA256(imagePath); err != nil {
			return "", err
		}
	}
	if snapLoad != "" {
		id, err := loadSnapshot(m, snapLoad)
		if err != nil {
			return "", err
		}
		if imageID != "" && id != imageID {
			return "", fmt.Errorf("-snap-load: snapshot was taken with a different image (sha256 %s, %s is %s)",
				id, imagePath, imageID)
		}
		fmt.Fprintf(os.Stderr, "cerulean: %s: resumed from %s at step %d, PC %08X\n",
			m.Name(), snapLoad, m.Steps(), m.CPU().PC())
		return id, nil
	}
	img, err := loader.Load(imagePath, nb0Base)
	if err != nil {
		return "", err
	}
	if err := m.LoadImage(img); err != nil {
		return "", err
	}
	rtcTime := time.Now()
	if rtcFlag != "" {
		if rtcTime, err = time.ParseInLocation("2006-01-02T15:04:05", rtcFlag, time.Local); err != nil {
			return "", fmt.Errorf("-rtc: %w", err)
		}
	}
	m.SetRTC(rtcTime)
	m.Reset()
	fmt.Fprintf(os.Stderr, "cerulean: %s: loaded %s image, entry %08X (PA %08X)\n",
		m.Name(), img.Format, img.Entry, m.CPU().PC())
	return imageID, nil
}
