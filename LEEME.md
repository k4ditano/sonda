<div align = center>

<img src="assets/header.svg" width="750" alt="Sonda">

<br>

**Tus archivos, y una sonda que los encuentra por lo que contienen.**

[English](README.md) · [Discord](https://discord.gg/N7kbYC49b2) · [Ko-fi](https://ko-fi.com/k4ditano)

</div>

Sonda es un gestor de archivos para [pleamar-wm](https://github.com/k4ditano/pleamar-wm)
cuya búsqueda sabe qué hay dentro de cada archivo:
- una foto, por lo que muestra;
- un PDF, en la página que lo dice;
- una grabación, en el minuto exacto;
- un vídeo, en el momento exacto;
- el código, en la línea.

Lo describes con tus palabras, en cualquier idioma, y lo saca a la luz. Todo
ocurre en tu ordenador.

Usa [EmbeddingGemma 2](https://huggingface.co/google/embeddinggemma-2), el
modelo multimodal de Google DeepMind, sobre [llama.cpp](https://github.com/ggml-org/llama.cpp)
en la tarjeta gráfica con Vulkan. Su ventana es una escena de
[pleamar](https://github.com/k4ditano/pleamar). La forma de leer los archivos y
de ordenar los resultados sigue a [DigUp](https://github.com/ARahim3/DigUp) (MIT).

## Qué hace

- **Gestor de archivos**:
  - tus lugares, y las carpetas en cuadrícula con miniaturas reales (fotos, la primera página de un PDF, un fotograma de un vídeo) o en lista;
  - atrás, adelante, arriba, migas de pan y una barra de ubicación para escribir la ruta (Ctrl+L, o un clic junto a las migas): `~` es tu carpeta personal, Tab completa el nombre de una carpeta, y la ruta de un archivo abre su carpeta con él elegido;
  - copiar, cortar, pegar, renombrar, carpeta nueva, papelera y restaurar;
  - arrastrar archivos a cualquier programa, y soltarlos desde cualquier programa sobre la carpeta, una carpeta de dentro, un lugar, una miga o una unidad;
  - tus unidades: un pincho se monta al pulsarlo y tiene su botón de expulsar;
  - las copias grandes dicen cuánto llevan y se pueden parar a medias (no queda nada a medio copiar); en btrfs o xfs copiar es instantáneo;
  - si un nombre ya existe, pregunta: reemplazar, saltar o mantener ambos (lo reemplazado va a la papelera);
  - **Ctrl+Z deshace** lo último: copiar, mover, renombrar, carpeta nueva, papelera, comprimir o extraer;
  - **Abrir con…** cualquier programa, una vez o siempre para ese tipo de archivo;
  - **Propiedades**: tamaño (el de una carpeta, contado), fechas, dueño, permisos (y cambiarlos) y espacio libre en el disco;
  - **comprimir** en zip (o tar.gz, 7z… según el nombre) y **extraer aquí** zip, rar, 7z, tar.* o iso;
  - una terminal en la carpeta.
- **Búsqueda por significado y por palabras**:
  - escribe «el contrato del piso», «un perro en la playa» o «donde hablan del sueño»;
  - las palabras exactas también cuentan: números de factura, nombres y códigos;
  - puedes filtrar por tipo y buscar en todo o solo en la carpeta donde estás.
- **Encontrado donde está**:
  - cada resultado dice dónde: página 5, en el 1:23, línea 120;
  - la vista previa muestra esa página, ese fotograma o ese trozo de texto, con tus palabras en negrita;
  - un vídeo o una grabación **se reproducen ahí mismo, dentro de la vista previa**, desde ese momento.
- **Código**: empieza con `code:` y di qué hace el código. Lee tus proyectos como los ve git, y nunca claves ni contraseñas.
- **Siempre al día**: el lector trabaja en segundo plano (`sonda daemon`), con la prioridad más baja.

## El gestor de archivos del escritorio

`sonda setup` pone a Sonda en el bus de la sesión:
- **«Mostrar en la carpeta»** de un navegador o de un chat abre Sonda en esa carpeta, con el archivo elegido.
- **El diálogo «Abrir» y «Guardar»** de los programas que se lo piden a xdg-desktop-portal es la propia ventana de Sonda, con su búsqueda: «la factura de la luz de marzo», y adjuntarla. Necesita, una vez y como root, este archivo; `install.sh` lo pide con sudo si lo ejecutas en una terminal, y si no:

  ```sh
  sudo install -Dm644 data/sonda.portal /usr/share/xdg-desktop-portal/portals/sonda.portal
  ```

  y en `~/.config/xdg-desktop-portal/pleamar-portals.conf`, bajo `[preferred]`, `org.freedesktop.impl.portal.FileChooser=sonda`; después `systemctl --user restart xdg-desktop-portal`.

  Al elegir, la ventana se titula «Sonda · Elegir», para que flote sobre el programa como un diálogo de archivos: en el `session.conf` de pleamar-wm, `window title="Sonda · Elegir" float`.

Para abrir las carpetas con ella: `xdg-mime default sonda.desktop inode/directory`.

## Teclas

| | |
| --- | --- |
| `Super+E` · `Super+S` | Sonda · Sonda con la búsqueda lista |
| escribir sobre una carpeta | empieza una búsqueda |
| flechas · `Intro` | moverse · abrir |
| `Alt+←` `Alt+→` `Alt+↑` · `Retroceso` | atrás, adelante, arriba · atrás |
| `Ctrl+C` `Ctrl+X` `Ctrl+V` | copiar, cortar, pegar |
| `F2` · `Ctrl+Mayús+N` · `Supr` | renombrar · carpeta nueva · a la papelera |
| `Ctrl+L` | la barra de ubicación: escribe una ruta (`Tab` completa) |
| `Ctrl+H` · `Espacio` · `Ctrl+A` | ocultos · vista previa · todo |
| `Ctrl+Z` · `Alt+Intro` | deshacer · propiedades |
| `Esc` | sale de la búsqueda |

## Instalar

```sh
sh scripts/build-llama.sh
sh install.sh
```

Lo que lee está en `~/.config/sonda/config.json`. Desde la terminal tienes
`sonda search …`, `sonda status`, `sonda pause` y `sonda resume`.

Licencia MIT. Las contribuciones son bienvenidas, hechas con IA o sin ella: mira la [política de IA](AI_POLICY.md).
