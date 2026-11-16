# Kimera

**A esteira do fotógrafo para os apps Craft.**

[English](README.md) · Português (Brasil) · [Roadmap](ROADMAP.md) · [Changelog](CHANGELOG.md)

Funciona com LightCraft, PhotoCraft e VectorCraft, da equipe ArtCraft. Não é afiliado nem
endossado pela equipe ArtCraft.

Escolha uma pasta de fotos e diga o que quer ("tira as espinhas, clareia os dentes, dá um look de
cinema e faz uma versão stories"). A sua IA transforma isso numa receita. O Kimera então roda as
ferramentas de linha de comando dos próprios apps Craft, uma depois da outra, em cada foto, como
uma esteira:

```
sua pasta (só leitura)
  └─► work/run-<hora>/
        1-photocraft/            limpeza, trabalho de pixel  (photocraft-cli run)
        2-lightcraft/            máscaras, cor, retoque      (lightcraft-cli run --import … app.export)
        3-stories-lightcraft/    uma pasta por variante      (feed, stories, PB…)
```

Os apps rodam sem janela, então nenhum precisa estar aberto. Os originais nunca são alterados,
cada execução tem a sua pasta, e uma foto que falha para ali enquanto as outras continuam.

## Instalação

- **Zip e instalador para Windows:** `scripts\package.ps1` gera
  `dist\Kimera-<versão>-windows-x64.zip`; compilar `installer\kimera.iss` no Inno Setup 6 cria o
  instalador, que põe o Kimera no menu Iniciar e, se você quiser, um "Open in Kimera" no clique
  direito das pastas. Versões marcadas (`v*`) são compiladas para Windows, macOS e Linux pelo
  workflow do GitHub.
- **Pelo código-fonte:** `cargo build --release` gera `target/release/kimera.exe` (linha de
  comando) e `target/release/kimera-gui.exe` (janela).

Abra o **Kimera** (a janela). Na primeira vez ele mostra o **Setup**:

1. Os caminhos do `lightcraft-cli`, do `photocraft-cli` e, se quiser, do `vectorcraft-cli`.
2. Uma pasta de trabalho para as execuções.
3. A IA: **Anthropic** ou **OpenAI** (chave em `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`, ou colada
   no campo), ou **Claude Code** / **Codex** (os CLIs `claude` / `codex` logados neste computador).
   `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` são respeitadas para gateways.
4. **Save**. As verificações mostram o que funciona e quais modelos opcionais do LightCraft
   faltam: SAM 3 para máscaras com IA, YuNet para tirar espinhas automaticamente, base de lentes.
   Cada um pode ser instalado dali, por um `lightcraft --control 7980` aberto.

As configurações ficam em `kimera.toml`, ao lado do programa (veja `kimera.toml.example`).

## A janela

1. **Photos:** escolha uma pasta (ou clique direito numa pasta → *Open in Kimera*) e marque uma,
   algumas ou todas as fotos.
2. **What should happen?** Descreva a edição e clique em *Write recipe with AI*. A IA lê as
   listas reais de comandos dos apps e o [playbook](prompts/playbook.md) do Kimera, e responde
   com notas e uma receita. O Kimera recusa qualquer comando que os apps não tenham. *Change
   current recipe* ajusta a receita da tela em vez de começar do zero.
3. **Recipe:** leia ou edite, ou abra uma da pasta `recipes/`.
4. **Test on 1 photo** mostra antes e depois. **Run on N photos** abre o quadro da esteira
   (fila → cada etapa → pronta / falhou), com log ao vivo e *Cancel*, que para o app na hora.
   *AI looks at each photo first* acha olho vermelho (corrigido no LightCraft) e olho fechado
   (marcado para revisão, com fotos vizinhas das mesmas pessoas para trocar os olhos).
5. **Client proof (PDF)** gera folhas de contato A4 com os resultados.

## A linha de comando

| Comando | O que faz |
|---|---|
| `kimera doctor` | Confere os apps, os modelos, a pasta de trabalho e a IA |
| `kimera ask "<pedido>" <pasta ou arquivos> [--run] [--analyse]` | A IA escreve a receita (salva, mostra e roda com `--run`) |
| `kimera run --recipe <arquivo> <pasta ou arquivos> [--analyse]` | Roda uma receita |
| `kimera check <receitas…>` | Confere receitas contra os comandos reais dos apps e os modelos instalados |
| `kimera analyse <pasta ou arquivos>` | A IA olha cada foto: rostos, olhos fechados, olho vermelho |
| `kimera proof <pasta ou arquivos>` | Prova do cliente: folhas de contato A4 em PDF |
| `kimera pipeline --recipe <arquivo> [--ids 12,15] [--reimport]` | Exporta fotos de um LightCraft aberto, roda a receita e, com `--reimport`, empilha os resultados nos originais |
| `kimera unimport <pasta da execução>` | Desfaz um `--reimport`: os resultados importados vão para a lixeira do LightCraft (os arquivos ficam) |
| `kimera models install sam3\|yunet\|lensdb --accept` | Instala um modelo opcional do LightCraft pelo app aberto |
| `kimera recipes` | Lista as receitas que vêm com o Kimera |

## Receitas

Uma receita é um arquivo TOML: tabelas `[[stage]]` rodam em ordem, cada uma com um app e os
comandos dele. Tabelas `[[variant]]` geram vários resultados por foto. Para listar os comandos:
`lightcraft-cli commands`, `photocraft-cli commands` ou `vectorcraft-cli commands`.

```toml
[[stage]]
app = "lightcraft"
steps = [
  { cmd = "spot.findBlemishes", params = { sensitivity = 45 }, optional = "blemishes" },
  { cmd = "preset.apply", params = { id = "lc.cinematic", amount = 80 } },
  { cmd = "mask.add", params = { kind = "prompt", text = "teeth" }, optional = "teeth" },
  { cmd = "mask.adjust", params = { values = { exposure = 0.15, saturation = -35 } }, optional = "teeth" },
]
export = { format = "tif", bit_depth = 16 }

[[variant]]
name = "stories"
[[variant.stage]]
app = "lightcraft"
steps = [{ cmd = "crop.aspect", params = { aspect = [9, 16] } }]
export = { format = "jpg", long_edge = 1920, options = { watermark = { text = "© Studio" } } }
```

- `optional = "<grupo>"`: quando um passo do grupo falha numa foto (não há dentes na foto, falta
  um modelo), o grupo inteiro sai daquela foto e a etapa roda de novo.
- `export` aceita `format`, `long_edge`, `quality`, `bit_depth`, `color_space` e `options` para
  qualquer outra opção de exportação do LightCraft (marca d'água, `limitKb`, `sharpen`,
  `metadata`, `removeLocation`…). Opções que mudam onde o arquivo é salvo são recusadas.
- Etapas do PhotoCraft gravam `out_ext` (`tif` entre etapas, `pcraft` só na última).
- Etapas do VectorCraft abrem a foto, ou um `template` cujos passos a posicionam com `{photo}`.
- Arquivos raw precisam passar por uma etapa do LightCraft antes de qualquer etapa do PhotoCraft.

Receitas incluídas: limpeza cinematográfica, retoque de retrato, formatos de rede social (feed,
stories, quadrado), fundo branco de marketplace, preto e branco, galeria web com marca d'água,
entrega para impressão, imobiliário e máscara de sujeito.

## Regras de projeto

- **Nunca travar:** nada de `unwrap`, `expect` ou `panic!` fora dos testes, e nenhum `unsafe`.
  Toda falha vira uma mensagem de erro.
- **Não destrutivo:** uma etapa nunca grava por cima da entrada, os originais são só lidos, e
  cada execução ganha uma pasta nova.
- **Receitas conferidas:** receitas podem vir de uma IA, então ids de comando, extensões e opções
  de exportação são conferidos antes de qualquer coisa rodar, e só os comandos reais dos apps são
  aceitos.
- **Só conexões locais:** o Kimera fala com o canal de controle do LightCraft apenas em
  `127.0.0.1`.
- **Sua IA, suas chaves:** os pedidos e as miniaturas vão só para o provedor de IA que você
  configurar.

## Licença

MIT OR Apache-2.0.
