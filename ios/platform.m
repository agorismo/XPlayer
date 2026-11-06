#import <UIKit/UIKit.h>
#import <AVFoundation/AVFoundation.h>

// Categoria "Playback": toca mesmo com a chave de silêncio ligada e permite áudio em segundo plano
// (junto com UIBackgroundModes=audio no Info.plist).
void meu_player_setup_audio_session(void) {
    AVAudioSession *s = [AVAudioSession sharedInstance];
    NSError *err = nil;
    [s setCategory:AVAudioSessionCategoryPlayback error:&err];
    [s setActive:YES error:&err];
}

// out = [top, bottom, left, right] em pontos. Chamar da thread principal.
void meu_player_safe_insets(float *out) {
    out[0] = out[1] = out[2] = out[3] = 0;
    for (UIScene *sc in UIApplication.sharedApplication.connectedScenes) {
        if (![sc isKindOfClass:[UIWindowScene class]]) continue;
        for (UIWindow *w in ((UIWindowScene *)sc).windows) {
            if (w.isKeyWindow) {
                UIEdgeInsets i = w.safeAreaInsets;
                out[0] = i.top; out[1] = i.bottom; out[2] = i.left; out[3] = i.right;
                return;
            }
        }
    }
}
