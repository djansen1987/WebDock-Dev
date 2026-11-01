ARG PHP_VERSION=8.3
FROM php:${PHP_VERSION}-fpm-alpine

RUN apk add --no-cache icu-dev libzip-dev libpng-dev libjpeg-turbo-dev freetype-dev oniguruma-dev git unzip \
 && docker-php-ext-configure gd --with-freetype --with-jpeg \
 && docker-php-ext-install -j"$(nproc)" pdo_mysql mysqli gd zip intl bcmath exif mbstring \
 && { php -v | grep -qi opcache || php -m | grep -qi opcache || docker-php-ext-install opcache; }

COPY --from=composer:2 /usr/bin/composer /usr/local/bin/composer

RUN { echo "memory_limit=512M"; echo "upload_max_filesize=128M"; echo "post_max_size=128M"; echo "display_errors=On"; } \
    > /usr/local/etc/php/conf.d/zz-dev.ini

WORKDIR /var/www
