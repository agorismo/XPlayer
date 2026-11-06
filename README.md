# Meu Player (Rust) — desktop + iOS

Player de áudio estilo foobar2000, reescrito em Rust (egui + symphonia + cpal), com sistema de plugins.
Usa a mesma pasta de dados da versão Electron (`%APPDATA%\meu-player`), então `session.json` e `config.json` antigos (playlists, EQ, plugins) continuam valendo.

## Compilar e rodar (Windows)
1. Instale o Rust: https://rustup.rs (toolchain MSVC; precisa das "Build Tools do Visual Studio" com C++).
2. Na pasta do projeto:

    cargo run --release

O executável fica em `target\release\meu-player.exe`. Arquivos de áudio passados na linha de comando ("Abrir com…") são adicionados à playlist.
No Linux é preciso `libasound2-dev` e `pkg-config`.

## Formatos
mp3, flac, ogg (Vorbis), wav, m4a/aac/mp4 (AAC, ALAC), aiff, caf.
**Não suportados** (limitação do decodificador): Opus e WMA. Arquivos desses formatos não são adicionados nas pastas e, se forem tocados, dão erro e a lista pula para a próxima.

## Recursos
- Playlists em abas (duplo clique renomeia), busca (Ctrl+F), ordenar clicando nas colunas, Delete remove faixas, Ctrl+A seleciona tudo, arrastar e soltar arquivos/pastas
- Equalizador de 10 bandas + pré-amp + presets, ReplayGain (faixa/álbum, pré-amp, evitar clipping)
- Playlists, EQ, volume e ReplayGain salvos ao fechar
- Plugins: Discord Rich Presence e Last.fm (configure em **Plugins**; mesmas configurações da versão anterior)

## Capas locais (novo)
Botão **Capas** na barra superior, clique direito na capa ou numa faixa, ou soltar uma imagem na janela.
- **Escolher imagem** para a faixa ou para o álbum inteiro (jpg, png, webp, bmp, gif). Por padrão a imagem é copiada para a pasta do player (`covers\`), então continua valendo se você mover o original.
- **Soltar uma imagem** na janela aplica a capa à faixa tocando (álbum ou só faixa, conforme a configuração).
- **Ordem de prioridade configurável** (ativar/desativar e subir/descer): capa escolhida por você → embutida no arquivo → imagem na pasta da música (`cover`, `folder`, `front`, `albumart`, `album`, `art` — nomes editáveis) → biblioteca central de capas (`Artista - Álbum.jpg` ou `Álbum.jpg`).
- **Imagem padrão** usada quando nada é encontrado; tamanho máximo de carregamento ajustável.
- **Salvar capa atual na pasta da música** (cria `cover.jpg/png`, sem sobrescrever).
- **Remover capa personalizada** e **Esquecer todas**.

## Plugins
- discord-rich: precisa do Application ID (discord.com/developers/applications). A capa no Discord vem do MusicBrainz/Cover Art Archive (o Discord só aceita URLs), não das capas locais.
- lastfm: precisa de API key/secret, usuário e senha (a senha é apagada após o login)
Os plugins agora são módulos Rust em `src/plugins/` (trait `Plugin`); para criar outro, implemente a trait e registre em `PluginHost::new`.

## Observações
- Reamostragem para a taxa do dispositivo é linear (simples); se o dispositivo tocar na taxa do arquivo, não há reamostragem.
- Testes: `cargo test` (EQ, ReplayGain, capas, decodificação; os testes com arquivos de mídia são ignorados se não existirem).

## iOS (.ipa)
O app também compila para iPhone/iPad (mesma base de código; layout se adapta a telas estreitas).
**Atenção:** não existe Mac aqui, então o build iOS **nunca foi compilado nem testado em aparelho** — só o layout estreito foi conferido no desktop (`cargo run --features mobile` e redimensione a janela). Se o GitHub Actions apontar erro, me mande o log.

### Gerar o .ipa (sem Mac, grátis)
1. Crie um repositório no GitHub (pode ser privado) e envie esta pasta inteira (inclui `.github/workflows/ios.yml`).
2. No GitHub: **Actions → "Gerar IPA (iOS)" → Run workflow**. Leva uns 10–15 min.
3. Baixe o artefato **MeuPlayer-ipa** (um zip com `MeuPlayer.ipa`). O .ipa sai **sem assinatura**.

### Instalar no iPhone
- Com **Sideloadly** (Windows/Mac) ou **AltStore**: conecte o iPhone, arraste o `.ipa` e entre com seu Apple ID. Conta gratuita: o app expira em **7 dias** (é só reinstalar); conta de desenvolvedor paga: 1 ano.
- No iPhone ative o **Modo de Desenvolvedor** (Ajustes → Privacidade e Segurança) e confie no perfil (Ajustes → Geral → VPN e Gerenciamento).

### Usar
- Copie músicas/imagens para o app: **Arquivos → No meu iPhone → Meu Player** (ou pelo Compartilhamento de Arquivos do iTunes/Finder). Depois toque em **+ Importar**.
- Capas: segure a capa pequena (barra de baixo) → escolher imagem para a faixa/álbum (lista as imagens da pasta do app). Pasta **Capas** dentro do app = biblioteca central (`Artista - Álbum.jpg`).
- Um toque numa música toca. Menu → Capas / Áudio (EQ) / Plugins.

### Limitações no iOS
- Sem Discord Rich Presence (usa IPC do desktop). Last.fm funciona.
- Áudio continua com a tela bloqueada, mas **a troca automática para a próxima faixa em segundo plano pode não funcionar** (a interface é pausada pelo iOS); sem controles na tela de bloqueio ainda.
- Sem ícone próprio; margens do notch vêm de uma pequena ponte Objective-C (`ios/platform.m`).
- Caminhos de arquivos mudam a cada reinstalação; o app tenta reencontrar os arquivos dentro da pasta do app.
