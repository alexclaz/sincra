# Sincra

Sincronização automática de multicâmera por áudio. Você importa a timeline "crua"
(XML Final Cut Pro 7, exportado do DaVinci Resolve ou do Premiere) e ele devolve a timeline com cada
câmera alinhada ao gravador de áudio, em `<nome> - synced.xml`.

Binário nativo em Rust para **Windows e macOS** (Intel e Apple Silicon), com interface gráfica e linha
de comando. O ffmpeg vem junto no pacote.

## Instalação

**macOS** (Terminal):

```bash
curl -fsSL https://raw.githubusercontent.com/alexclaz/sincra/main/install.sh | sh
```

Instala `Sincra.app` em `~/Applications` e o comando `sincra` em `~/.local/bin`.
O app não é assinado nem notarizado pela Apple. Como ele é baixado pelo `curl`, e não pelo navegador,
o macOS não marca o arquivo como "baixado da internet" e o Gatekeeper não bloqueia.

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/alexclaz/sincra/main/install.ps1 | iex
```

Instala em `%LOCALAPPDATA%\Programs\Sincra`, cria o atalho no menu Iniciar e coloca o
`sincra` no PATH. Não pede administrador e o SmartScreen não bloqueia (mesmo motivo do macOS).

Para instalar uma versão específica, defina `SINCRA_VERSION=v0.2.0`. Para desinstalar:

```bash
curl -fsSL https://raw.githubusercontent.com/alexclaz/sincra/main/install.sh | SINCRA_UNINSTALL=1 sh
```

```powershell
$env:SINCRA_UNINSTALL=1; irm https://raw.githubusercontent.com/alexclaz/sincra/main/install.ps1 | iex
```

## Uso

### Interface

1. **Importar XML**, arrastar o arquivo para a janela ou escolher um dos recentes. Se o XML tiver mais de
   uma sequência, escolha ao lado do nome.
2. Escolha a **Precisão** na barra de cima (veja os modos abaixo). O padrão é *Ótimo*.
3. **Sincronizar**.
4. **Exportar XML** e importe o arquivo no Resolve ou no Premiere.

A tela principal é a timeline, com uma faixa por dispositivo. O cabeçalho da faixa mostra quantos clipes
casaram pelo áudio, e o anel à direita fixa aquele dispositivo como referência. O jeito do clipe indica
o resultado:

- **preenchido**: casou pelo áudio (quanto mais opaco, maior a confiança);
- **hachurado**: o áudio não casou e ele foi posicionado pelo horário de criação;
- **só contorno tracejado**: não sincronizado (ficou onde estava no XML) ou arquivo não encontrado.

Cada clipe mostra o deslocamento aplicado (por exemplo `+3,2 s`), e *Posição original* desenha onde ele
estava no XML importado. Clique num clipe para ver os detalhes no painel da direita. Ctrl + rolar dá
zoom, arrastar move, *Ajustar* mostra tudo. A barra de baixo mostra a última mensagem; *Log* abre o histórico.

Na engrenagem ficam o idioma (português ou inglês), o tema (claro ou escuro; os dois seguem o sistema por
padrão, e o botão de meio círculo na barra de cima alterna o tema), a ordem dos clipes, a cronologia, as cores de label, o sufixo do arquivo, o cache, o
redirecionamento de caminhos e as pastas onde procurar mídias que mudaram de lugar.

### Linha de comando

```bash
sincra "Timeline 1.xml"
```

```bash
sincra "Timeline 1.xml" -m thorough -o saida.xml
```

```bash
sincra "Timeline 1.xml" --remap "E:/Trabalhos=/Volumes/HD/Trabalhos"
```

```bash
sincra "Timeline 1.xml" --list
```

`--list` mostra as sequências e os dispositivos detectados. As mensagens saem no idioma do sistema; use
`--lang en` ou `--lang pt` para escolher. `sincra --help` lista todas as opções.
Sem argumentos, `sincra` abre a interface.

## Modos

| Precisão (`-m` na linha de comando) | O que faz |
|---|---|
| Aproximado (`approximate`) | Só pelo horário de criação dos arquivos, sem áudio (erro de ~1 s e depende dos relógios estarem certos). |
| Muito rápido (`very_fast`) | Uma passada grossa (1 kHz), sem refinamento fino. |
| Rápido (`fast`) | Passada grossa a 1 kHz + refinamento fino a 8 kHz. |
| Avançado (`advanced`) | Multi-passadas a 2 kHz, sem usar o relógio para restringir a busca. |
| **Ótimo** (`optimal`) | Multi-passadas usando o horário de criação para prever onde procurar. Indicado para mídia original. |
| Minucioso (`thorough`) | Como o Ótimo, mas não confia no relógio (proxies e arquivos baixados). |
| Máximo (`utmost`) | Minucioso + tentativas em várias resoluções (1/2/4 kHz). É o mais lento. |

## Como funciona

1. **Leitura do XML:** cada colocação de mídia (vídeo + canais de áudio linkados) vira um "clipe".
   Os clipes são agrupados em **dispositivos** pela trilha e pela assinatura do arquivo: resolução,
   extensão, fabricante e padrão do nome. Dispositivo com vídeo é câmera; só áudio é gravador.
2. **Horário de criação:** vem do XML lateral da Sony (`C0001M01.XML`), do BWF `bext` dos WAVs
   (data + `time_reference`, com precisão de amostra), do `creation_time` do container ou, em último
   caso, do mtime do arquivo.
3. **Áudio:** é decodificado uma vez via ffmpeg para mono a 8 kHz e guardado em cache (arquivos `.npy`
   na pasta de cache do sistema, em `sincra`).
4. **Referência:** o gravador mais longo. Os clipes dele são dispostos pelo horário de criação.
5. **Busca por áudio (GCC-PHAT):**
   - Passada global em todo o gravador. Os matches fortes e inequívocos viram *âncoras*.
   - Com as âncoras, o motor decide sozinho se o relógio da câmera é confiável e qual é a ordem
     cronológica certa (horário, nome ou XML). Âncoras que contradizem o relógio ou a ordem são descartadas.
   - Os clipes restantes são buscados em janelas restritas: a previsão do relógio e o intervalo entre os
     vizinhos já sincronizados.
   - Por fim, um refinamento a 8 kHz com interpolação sub-amostra.
6. **Sem match:** o clipe vai pelo horário de criação, usando o offset de relógio do vizinho
   sincronizado, ou logo após o clipe anterior se o relógio não for confiável.
   Esses clipes recebem o label **Rose** (configurável).
7. **Exportação:** a posição em frames é calculada em relação ao frame *arredondado* do clipe do
   gravador ao qual a câmera casou. Cada dispositivo ganha suas próprias trilhas, e clipes que se
   sobrepõem vão para trilhas extras. Efeitos, filtros e links do XML original são preservados.

## Desenvolvimento

```bash
cargo build --release
```

Gera `target/release/sincra` (CLI) e `sincra-gui` (interface; no Windows sem janela de
console). Em desenvolvimento, aponte o ffmpeg com a variável `SINCRA_FFMPEG` se ele não estiver no PATH.
O ffmpeg é procurado nesta ordem: `SINCRA_FFMPEG`, ao lado do executável, PATH, Homebrew.

### Publicar uma versão

O workflow [`.github/workflows/release.yml`](.github/workflows/release.yml) compila Windows x64,
macOS arm64 e macOS x64, junta o ffmpeg (dos wheels do `imageio-ffmpeg` no PyPI),
monta o `.app` com assinatura ad-hoc e publica a Release. Basta subir uma tag:

```bash
git tag v0.2.0
```

```bash
git push origin v0.2.0
```

O repositório precisa ser **público** para o `curl`/`irm` baixarem sem autenticação.

### Validação

O Sincra é uma reescrita em Rust de uma versão anterior em Python. Rodando as duas no mesmo XML, as saídas
são idênticas byte a byte:

- `Timeline 1.xml` (11-08 HIGOR NHOT, 3 câmeras + Zoom): 15/15 clipes no mesmo frame.
- 10-10 OLI (33 clipes Sony + 3 DJI + Zoom): 39/39 clipes no mesmo frame.

Tempo no `Timeline 1.xml`: 65 s nas duas versões com o cache vazio (o gargalo é o ffmpeg lendo os WAVs
do disco) e, com o cache pronto, 23 s na Rust contra 37 s na Python.

## Limitações

- Só lê e escreve **FCP7 XML (xmeml)**. Ainda não suporta FCPXML (Final Cut Pro X).
- Clipes com velocidade alterada ou mídia com drift grande (horas sem timecode) não recebem correção de drift.
- O áudio é mixado em mono. Gravadores com canal mudo funcionam, mas canais em fase invertida podem
  se cancelar.
- No macOS, "Abrir com" pelo Finder não passa o arquivo para o app; use IMPORTAR XML ou arraste para a janela.
