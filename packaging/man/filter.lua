-- Pandoc filter for tools/build-man.sh: turns a page header and the book
-- chapters after it into the sections of one man page.
--
-- The script puts "<!-- chapter: book/src/PATH -->" before each chapter and
-- "<!-- end of chapters -->" before the rest of the header. Blocks before the
-- first marker and after the last one are the header's, and stay as written.

-- Each chapter that goes into a man page, by its path under book/src: the
-- page, the chapter's title, and how its headings become sections.
--
-- A page's first chapter has no `section`: its title is dropped, its "##"
-- headings become sections and "###" subsections. What comes before its
-- first "##" is the page's SYNOPSIS (a code block) and DESCRIPTION; with
-- `synopsis`, the SYNOPSIS is a copy of the first code block under the "##"
-- heading of that anchor instead. Any other chapter becomes the one section
-- `section`, and its "##" headings its subsections.
--
-- The titles are checked against the chapters: a link whose text is a
-- chapter's title is rewritten to name the page or section instead.
local PAGE = {
  ["reference/command-line.md"] = { page = "irsync(1)", title = "Command line" },
  ["reference/management.md"] = {
    page = "irsync(1)", title = "Management commands", section = "MANAGEMENT COMMANDS" },
  ["reference/remote-addresses.md"] = {
    page = "irsync(1)", title = "Remote addresses", section = "REMOTE ADDRESSES" },
  ["reference/files-and-state.md"] = {
    page = "irsync(1)", title = "Files and state", section = "FILES" },
  ["reference/limitations.md"] = {
    page = "irsync(1)", title = "Known limitations", section = "LIMITATIONS" },
  ["reference/configuration.md"] = { page = "irsync.toml(5)", title = "Configuration" },
  ["fileset/reference.md"] = {
    page = "fileset(1)", title = "fileset reference", synopsis = "commands" },
}

-- Sections of irsync(1) that move to the end of the page, in this order,
-- before SEE ALSO, where man readers look for them. The book keeps its
-- chapter order.
local TAIL = { "ENVIRONMENT", "FILES", "EXIT STATUS", "LIMITATIONS" }

local BOOK = "https://irsync.org/book/"

-- Parts of the book that explain rather than instruct. A man page names
-- their pages as pages of the book, without a URL, so that the few URLs it
-- gives are the how-to guides and tutorials a reader acts on.
local IN_BOOK = { "explanation/", "contributing/" }

-- Tables whose first column holds one of these become tagged lists, the
-- man layout for options, commands, variables and configuration keys.
local TERMS = {
  ["Flag"] = true, ["Option"] = true, ["Command"] = true,
  ["Variable"] = true, ["Key"] = true, ["rsync option"] = true,
}

-- Where text starts at an 80-column terminal, and the last column it may
-- use: groff's line is 78 columns, man indents a section's text by 7.
local INDENT, LINE = 7, 78

-- pandoc writes a table column of width w as w * 70 columns.
local PANDOC_WIDTH = 70

local stringify = pandoc.utils.stringify

local function upper(inlines)
  return pandoc.text.upper(stringify(inlines))
end

-- The anchor mdbook gives a heading.
local function slug(inlines)
  local s = pandoc.text.lower(stringify(inlines))
  s = s:gsub("%s", "-"):gsub("[^%w_%-\128-\255]", "")
  return s
end

-- Resolves a relative link path against a chapter's directory.
local function resolve(dir, path)
  local parts = {}
  for part in (dir .. path):gmatch("[^/]+") do
    if part == ".." then
      table.remove(parts)
    elseif part ~= "." then
      table.insert(parts, part)
    end
  end
  return table.concat(parts, "/")
end

-- The man heading of each heading of the page's chapters, by chapter and
-- anchor. "" stands for the chapter itself.
local function headings(chapters)
  local map = {}
  for _, c in ipairs(chapters) do
    local spec = PAGE[c.path]
    local m = { [""] = spec.section }
    for _, b in ipairs(c.blocks) do
      if b.t == "Header" and b.level > 1 then
        local man
        if spec.section or b.level > 2 then
          man = stringify(b.content)
        elseif stringify(b.content) == "Exit codes" then
          man = "EXIT STATUS"
        else
          man = upper(b.content)
        end
        m[slug(b.content)] = man
      end
    end
    map[c.path] = m
  end
  return map
end

-- A section of this page by its man heading. A link whose text names the
-- section in other words, such as "the id commands" for "id", keeps it.
local function section_ref(text, man)
  local s = stringify(text)
  if pandoc.text.lower(s) ~= pandoc.text.lower(man)
      and (" " .. pandoc.text.lower(s) .. " "):find(" " .. pandoc.text.lower(man) .. " ", 1, true) then
    return text
  end
  return pandoc.Inlines({ pandoc.Str(man) })
end

-- Rewrites the links of one page. `state` holds the page, the headings of
-- its chapters and the book pages whose URL it has already given.
local function link(el, chapter, state, notes)
  local target = el.target
  if target:match("^%a[%w+.-]*:") then
    -- An absolute URL stays. An autolink is written as plain text: as a
    -- link with no text of its own, mandoc warns about an empty block.
    if stringify(el.content) == target then
      return pandoc.Str(target)
    end
    return el
  end
  local path, anchor = target:match("^([^#]*)#?(.*)$")
  if not chapter then
    return el
  elseif path == "" then
    path = chapter
  elseif not path:match("%.md$") then
    return el
  else
    path = resolve(chapter:match("^(.*/)") or "", path)
  end
  local text = el.content
  local spec = PAGE[path]
  if spec and spec.page == state.page then
    local man = state.headings[path] and state.headings[path][anchor]
    if not man then
      error("filter.lua: book/src/" .. chapter .. " links to " .. target
        .. ", which is not a heading of " .. state.page)
    end
    return section_ref(text, man)
  end
  if spec then
    -- "See [Configuration](configuration.md)" becomes "See irsync.toml(5)",
    -- "[Remote addresses](remote-addresses.md#x)" "REMOTE ADDRESSES in
    -- irsync(1)"; other text is followed by the page.
    if stringify(text) == spec.title then
      if spec.section then
        return pandoc.Inlines({ pandoc.Str(spec.section .. " in " .. spec.page) })
      end
      return pandoc.Inlines({ pandoc.Str(spec.page) })
    end
    text:insert(pandoc.Space())
    text:insert(pandoc.Str("(" .. spec.page .. ")"))
    return text
  end
  -- "See [Seed the index](...)" becomes 'See "Seed the index" in the
  -- book': the quotes keep "in the book" from reading as part of a title
  -- such as "Seed the index".
  local function in_book()
    return pandoc.Inlines({ pandoc.Quoted("DoubleQuote", text), pandoc.Space(),
      pandoc.Str("in"), pandoc.Space(), pandoc.Str("the"), pandoc.Space(),
      pandoc.Str("book") })
  end
  for _, part in ipairs(IN_BOOK) do
    if path:sub(1, #part) == part then
      return in_book()
    end
  end
  -- Another page of the book: the first link to it gives its URL, after
  -- the block it is in (a URL is wider than most lines). Later links say
  -- it is in the book, as the explanations do.
  if state.linked[path] then
    return in_book()
  end
  state.linked[path] = true
  local url = BOOK .. path:gsub("%.md$", ".html") .. (anchor ~= "" and "#" .. anchor or "")
  table.insert(notes, { text = text:clone(), url = url })
  return text
end

-- The paragraph after a block that gives the URLs of its links.
local function link_notes(notes)
  local para = pandoc.Inlines({})
  for i, note in ipairs(notes) do
    if i > 1 then
      para:insert(pandoc.LineBreak())
    end
    para:extend(note.text)
    para:insert(pandoc.Str(":"))
    para:insert(pandoc.Space())
    para:insert(pandoc.Str(note.url))
  end
  return pandoc.Para(para)
end

-- The inlines of a table cell, or nil when it holds more than one paragraph.
local function cell_inlines(cell)
  if #cell.contents == 0 then
    return pandoc.Inlines({})
  end
  if #cell.contents == 1 and (cell.contents[1].t == "Plain" or cell.contents[1].t == "Para") then
    return cell.contents[1].content
  end
end

-- The width of each column of a table, its longest cell, and whether
-- every row fits on one line: tbl puts 3 columns between table columns
-- and 1 after the last.
local function measure_table(t)
  local width = {}
  local function measure(rows)
    for _, row in ipairs(rows) do
      for i, cell in ipairs(row.cells) do
        width[i] = math.max(width[i] or 0, utf8.len(stringify(cell.contents)))
      end
    end
  end
  measure(t.head.rows)
  for _, body in ipairs(t.bodies) do
    measure(body.body)
  end
  local total = INDENT + 3 * (#width - 1) + 1
  for _, w in ipairs(width) do
    total = total + w
  end
  return width, total <= LINE
end

-- A table as a tagged list: the first cell of a row is the tag. In a
-- table of keys, the default comes first in the text ("Default: 30m.");
-- in another table of more than two columns, each further cell is a line
-- of its own under its column's header ("irsync: ...", "rsync: ...").
-- A line break in a list item is .br: pandoc's would end the item.
local function as_list(t)
  local head = t.head.rows[1].cells
  local label = {}
  for i, cell in ipairs(head) do
    label[i] = cell_inlines(cell)
    if not label[i] then
      return nil
    end
  end
  local key_table = #head == 3 and stringify(label[2]) == "Default"
  local items = {}
  for _, body in ipairs(t.bodies) do
    for _, row in ipairs(body.body) do
      local cells = {}
      for i, cell in ipairs(row.cells) do
        cells[i] = cell_inlines(cell)
        if not cells[i] then
          return nil
        end
      end
      local text = pandoc.Inlines({})
      if #cells == 2 then
        text:extend(cells[2])
      elseif key_table and stringify(cells[2]):lower() == "required" then
        -- A key with no default: "Required.", not "Default: required."
        text:extend({ pandoc.Str("Required.") })
        text:insert(pandoc.Space())
        text:extend(cells[3])
      elseif key_table then
        text:extend({ pandoc.Str("Default:"), pandoc.Space() })
        text:extend(cells[2])
        if not stringify(cells[2]):match("%.$") then
          text:insert(pandoc.Str("."))
        end
        text:insert(pandoc.Space())
        text:extend(cells[3])
      else
        for i = 2, #cells do
          if i > 2 then
            text:insert(pandoc.RawInline("man", "\n.br\n"))
          end
          text:extend(label[i])
          text:extend({ pandoc.Str(":"), pandoc.Space() })
          text:extend(cells[i])
        end
      end
      table.insert(items, { cells[1], { { pandoc.Para(text) } } })
    end
  end
  return pandoc.DefinitionList(items)
end

-- A table of options, commands, variables or keys becomes a tagged list,
-- and so does any other table too wide for one line per row. A table
-- that stays gets each column as wide as its longest cell: without a
-- width, tbl wraps a cell at a fraction of the line.
local function convert_table(t)
  local width, fits = measure_table(t)
  if #t.head.rows == 1 then
    local first = stringify(t.head.rows[1].cells[1].contents)
    if TERMS[first] or not fits then
      local list = as_list(t)
      if list then
        return list
      end
    end
  end
  if not fits then
    error("filter.lua: a table too wide for a man page: " .. stringify(t.head))
  end
  for i, spec in ipairs(t.colspecs) do
    t.colspecs[i] = { spec[1], width[i] / PANDOC_WIDTH }
  end
  return t
end

-- Roff for a synopsis: its lines as they are, at the section's indent.
local function synopsis(code)
  local lines = {}
  for line in (code.text .. "\n"):gmatch("(.-)\n") do
    line = line:gsub("\\", "\\e"):gsub("%-", "\\-")
    if line:match("^[.']") then
      line = "\\&" .. line
    end
    table.insert(lines, line)
  end
  return pandoc.RawBlock("man", ".nf\n" .. table.concat(lines, "\n") .. "\n.fi")
end

-- Splits the blocks at the script's markers: the header before the
-- chapters, the chapters, and the header after them.
local function split(blocks)
  local before, chapters, after = pandoc.Blocks({}), {}, pandoc.Blocks({})
  local current = before
  for _, b in ipairs(blocks) do
    local path = b.t == "RawBlock" and b.text:match("^<!%-%- chapter: book/src/(%S+) %-%->%s*$")
    if path then
      if not PAGE[path] then
        error("filter.lua: no man page for chapter book/src/" .. path)
      end
      current = pandoc.Blocks({})
      table.insert(chapters, { path = path, blocks = current })
    elseif b.t == "RawBlock" and b.text:match("^<!%-%- end of chapters %-%->%s*$") then
      current = after
    else
      current:insert(b)
    end
  end
  for _, c in ipairs(chapters) do
    local title = c.blocks[1]
    local want = PAGE[c.path].title
    if not (title and title.t == "Header" and title.level == 1) then
      error("filter.lua: book/src/" .. c.path .. " does not start with its title")
    end
    if stringify(title.content) ~= want then
      error("filter.lua: book/src/" .. c.path .. " is titled '" .. stringify(title.content)
        .. "', not '" .. want .. "'; update PAGE in packaging/man/filter.lua")
    end
  end
  return before, chapters, after
end

-- The first code block under the "##" heading with this anchor.
local function code_under(blocks, anchor)
  local inside = false
  for _, b in ipairs(blocks) do
    if b.t == "Header" and b.level == 2 then
      inside = slug(b.content) == anchor
    elseif inside and b.t == "CodeBlock" then
      return b
    end
  end
  error("filter.lua: no code block under ## " .. anchor)
end

-- One chapter as man blocks.
local function chapter_blocks(c, state)
  local spec = PAGE[c.path]
  local out = pandoc.Blocks({})
  local intro = not spec.section -- before the first "##" of a first chapter
  local intro_section
  -- With `synopsis`, the code block copied to SYNOPSIS is left out of its
  -- own section, which would repeat it right under the SYNOPSIS. The book
  -- keeps it.
  local under_synopsis, synopsis_left_out = false, false
  if spec.synopsis then
    out:insert(pandoc.Header(1, "SYNOPSIS"))
    out:insert(synopsis(code_under(c.blocks, spec.synopsis)))
    intro_section = "SYNOPSIS"
  end
  for i, b in ipairs(c.blocks) do
    if b.t == "Header" and b.level == 2 then
      under_synopsis = spec.synopsis ~= nil and slug(b.content) == spec.synopsis
    end
    if under_synopsis and not synopsis_left_out and b.t == "CodeBlock" then
      synopsis_left_out = true
    elseif b.t == "Header" then
      local level = spec.section and b.level or b.level - 1
      if i == 1 then
        if spec.section then
          out:insert(pandoc.Header(1, spec.section))
        end
      else
        intro = false
        b.level = level
        if level == 1 then
          b.content = { pandoc.Str(state.headings[c.path][slug(b.content)]) }
        end
        out:insert(b)
      end
    else
      local notes = {}
      b = pandoc.walk_block(b, { Link = function(el) return link(el, c.path, state, notes) end })
      if b.t == "Table" then
        b = convert_table(b)
      end
      if intro then
        if not intro_section and b.t == "CodeBlock" then
          intro_section = "SYNOPSIS"
          out:insert(pandoc.Header(1, "SYNOPSIS"))
        elseif intro_section ~= "DESCRIPTION" and not (intro_section == "SYNOPSIS"
            and b.t == "CodeBlock" and not spec.synopsis) then
          intro_section = "DESCRIPTION"
          out:insert(pandoc.Header(1, "DESCRIPTION"))
        end
        if intro_section == "SYNOPSIS" then
          b = synopsis(b)
        end
      end
      out:insert(b)
      if #notes > 0 then
        out:insert(link_notes(notes))
      end
    end
  end
  return out
end

-- Moves the named level 1 sections, in the given order, to just before
-- SEE ALSO.
local function move_before_see_also(blocks, names)
  local sections, by_name = {}, {}
  local current = {}
  for _, b in ipairs(blocks) do
    if b.t == "Header" and b.level == 1 then
      current = { title = stringify(b.content), blocks = pandoc.Blocks({}) }
      table.insert(sections, current)
      by_name[current.title] = current
    end
    current.blocks:insert(b)
  end
  local moved = {}
  for _, name in ipairs(names) do
    moved[name] = true
  end
  local result = pandoc.Blocks({})
  for _, s in ipairs(sections) do
    if s.title == "SEE ALSO" then
      for _, name in ipairs(names) do
        if by_name[name] then
          result:extend(by_name[name].blocks)
        end
      end
    end
    if not moved[s.title] then
      result:extend(s.blocks)
    end
  end
  return result
end

function Pandoc(doc)
  local before, chapters, after = split(doc.blocks)
  local state = {
    page = PAGE[chapters[1].path].page,
    headings = headings(chapters),
    linked = {},
  }
  local out = pandoc.Blocks({})
  -- The header's own blocks; a code block in its SYNOPSIS is a synopsis.
  local function header(blocks)
    local in_synopsis = false
    for _, b in ipairs(blocks) do
      if b.t == "Header" and b.level == 1 then
        in_synopsis = stringify(b.content) == "SYNOPSIS"
      elseif in_synopsis and b.t == "CodeBlock" then
        b = synopsis(b)
      elseif b.t == "Table" then
        b = convert_table(b)
      end
      -- A header has no links into the book, only URLs.
      b = pandoc.walk_block(b, { Link = function(el) return link(el, nil, state, {}) end })
      out:insert(b)
    end
  end
  header(before)
  for _, c in ipairs(chapters) do
    out:extend(chapter_blocks(c, state))
  end
  header(after)
  -- Man section titles are upper case.
  for _, b in ipairs(out) do
    if b.t == "Header" and b.level == 1 then
      b.content = { pandoc.Str(upper(b.content)) }
    end
  end
  if state.page == "irsync(1)" then
    out = move_before_see_also(out, TAIL)
  end
  doc.blocks = out
  return doc
end
